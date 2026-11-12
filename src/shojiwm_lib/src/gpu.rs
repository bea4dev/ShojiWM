//! The GPUs of the seat, reported to the config (`COMPOSITOR.gpu`), and the
//! choice of the GPU everything is composited on (the render GPU).
//!
//! Everything here is read from sysfs, so a GPU can be described before it is
//! opened: the config picks the render GPU from these descriptions before any
//! GPU is opened (`select_render`).

use std::path::{Path, PathBuf};

/// PCI vendor ids.
const VENDOR_INTEL: u32 = 0x8086;
const VENDOR_AMD: u32 = 0x1002;
const VENDOR_NVIDIA: u32 = 0x10de;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum GpuVendor {
    Intel,
    Amd,
    Nvidia,
    Other,
}

impl GpuVendor {
    fn from_pci_id(vendor_id: u32) -> Self {
        match vendor_id {
            VENDOR_INTEL => Self::Intel,
            VENDOR_AMD => Self::Amd,
            VENDOR_NVIDIA => Self::Nvidia,
            _ => Self::Other,
        }
    }
}

/// One GPU, as the config sees it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GpuSnapshot {
    /// Where the GPU sits, e.g. `pci-0000:03:00.0` (udev's `ID_PATH`). It stays
    /// the same across reboots, so this is what a config should name.
    pub id: String,
    /// `card1`: numbered in probe order, so it can change between boots and
    /// when a GPU is plugged in again. For logs and display only.
    pub dynamic_card_id: String,
    /// `/dev/dri/renderD129`, numbered like [`Self::dynamic_card_id`].
    pub render_node: Option<String>,
    pub vendor: GpuVendor,
    /// PCI vendor and device ids; 0 for a GPU that is not on PCI.
    pub vendor_id: u32,
    pub device_id: u32,
    /// Kernel driver: `i915`, `xe`, `amdgpu`, `nvidia`, `nouveau`, ...
    pub driver: Option<String>,
    /// Model name from the hardware database, e.g. `Navi 32 [Radeon RX 7800 XT]`.
    pub name: Option<String>,
    /// The firmware showed the boot screen on this GPU.
    pub boot_vga: bool,
    /// The kernel reports the GPU as removable (Thunderbolt eGPU and the like).
    pub removable: bool,
    /// Connectors with a display connected, named like outputs (`DP-1`).
    pub outputs: Vec<String>,
    /// Everything is composited on this GPU. While the config picks the render
    /// GPU (`select_render`), this marks the one ShojiWM would pick on its own.
    pub is_render: bool,
    /// ShojiWM opened the GPU (it drives the GPU's displays and can composite
    /// on it). `SHOJI_TTY_DRM_DEVICE` can leave GPUs out.
    pub in_use: bool,
}

impl GpuSnapshot {
    /// Whether `choice` names this GPU: its [`Self::id`], the udev tag of it
    /// (`pci-0000_03_00_0`, as `DRI_PRIME` takes it), its card (`card1`,
    /// `/dev/dri/card1`) or its render node.
    pub fn matches(&self, choice: &str) -> bool {
        let choice = choice.trim();
        if choice.is_empty() {
            return false;
        }
        choice == self.id
            || choice == id_path_tag(&self.id)
            || choice == self.dynamic_card_id
            || choice == format!("/dev/dri/{}", self.dynamic_card_id)
            || self.render_node.as_deref() == Some(choice)
    }
}

/// The config's answer to "which GPU composites everything".
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RenderGpuChoice {
    /// ShojiWM's own pick (see `select_tty_devices` in the backend).
    #[default]
    Auto,
    /// A GPU named as [`GpuSnapshot::matches`] accepts.
    Gpu(String),
}

/// `SHOJI_RENDER_GPU`: picks the render GPU over the config (rescue, testing).
/// `auto` makes ShojiWM pick without asking the config.
pub fn render_gpu_env_override() -> Option<RenderGpuChoice> {
    let value = std::env::var("SHOJI_RENDER_GPU").ok()?;
    match value.trim() {
        "" => None,
        "auto" => Some(RenderGpuChoice::Auto),
        value => Some(RenderGpuChoice::Gpu(value.to_owned())),
    }
}

/// udev's `ID_PATH_TAG`: the id with everything but ASCII letters, digits
/// and `-` turned into `_`.
pub fn id_path_tag(id: &str) -> String {
    id.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

/// Environment that makes a program render on `gpu` (PRIME render offload).
///
/// The proprietary NVIDIA driver is chosen through GLVND and its Vulkan layer;
/// Mesa drivers (including nouveau) through `DRI_PRIME` for OpenGL and
/// `MESA_VK_DEVICE_SELECT` for Vulkan.
pub fn offload_env(gpu: &GpuSnapshot) -> Vec<(String, String)> {
    if gpu.driver.as_deref() == Some("nvidia") {
        return vec![
            ("__NV_PRIME_RENDER_OFFLOAD".into(), "1".into()),
            ("__GLX_VENDOR_LIBRARY_NAME".into(), "nvidia".into()),
            ("__VK_LAYER_NV_optimus".into(), "NVIDIA_only".into()),
        ];
    }
    let mut env = vec![("DRI_PRIME".into(), id_path_tag(&gpu.id))];
    if gpu.vendor_id != 0 {
        env.push((
            "MESA_VK_DEVICE_SELECT".into(),
            format!("{:04x}:{:04x}", gpu.vendor_id, gpu.device_id),
        ));
    }
    env
}

/// The `cardN` name of a DRM device path (`/dev/dri/card1`).
pub fn card_name(card_path: &Path) -> Option<&str> {
    card_path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| {
            name.strip_prefix("card")
                .is_some_and(|index| !index.is_empty() && index.bytes().all(|b| b.is_ascii_digit()))
        })
}

const SYSFS_DRM: &str = "/sys/class/drm";

/// Describe the GPU behind `card_path` (`/dev/dri/card1`) from sysfs. The
/// state ShojiWM keeps (`is_render`, `in_use`) is left `false`.
pub fn describe_gpu(card_path: &Path) -> GpuSnapshot {
    let card = card_name(card_path).unwrap_or_default();
    let mut gpu = describe_gpu_in(Path::new(SYSFS_DRM), card);
    gpu.name = hardware_database_name(&Path::new(SYSFS_DRM).join(card).join("device"));
    gpu
}

/// [`describe_gpu`] against a sysfs `class/drm` directory, without the
/// hardware database (which needs udev).
fn describe_gpu_in(sysfs_drm: &Path, card: &str) -> GpuSnapshot {
    let card_dir = sysfs_drm.join(card);
    let device = card_dir.join("device");
    let read = |name: &str| {
        std::fs::read_to_string(device.join(name))
            .ok()
            .map(|value| value.trim().to_owned())
    };
    let hex = |name: &str| {
        read(name)
            .and_then(|value| u32::from_str_radix(value.trim_start_matches("0x"), 16).ok())
            .unwrap_or(0)
    };
    let link_name = |path: PathBuf| {
        std::fs::read_link(path)
            .ok()
            .and_then(|target| target.file_name()?.to_str().map(str::to_owned))
    };
    let vendor_id = hex("vendor");
    let device_id = hex("device");
    let render_node = std::fs::read_dir(device.join("drm"))
        .ok()
        .into_iter()
        .flat_map(|entries| entries.filter_map(Result::ok))
        .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
        .find(|name| name.starts_with("renderD"))
        .map(|name| format!("/dev/dri/{name}"));
    GpuSnapshot {
        id: device_id_path(&device).unwrap_or_else(|| card.to_owned()),
        dynamic_card_id: card.to_owned(),
        render_node,
        vendor: GpuVendor::from_pci_id(vendor_id),
        vendor_id,
        device_id,
        driver: link_name(device.join("driver")),
        name: None,
        boot_vga: read("boot_vga").as_deref() == Some("1"),
        removable: read("removable").as_deref() == Some("removable"),
        outputs: connected_outputs_in(sysfs_drm, card),
        is_render: false,
        in_use: false,
    }
}

/// udev's `ID_PATH` of the device: `pci-<address>` for a PCI device (PCI
/// addresses are unique on their own), `<subsystem>-<name>` otherwise.
fn device_id_path(device: &Path) -> Option<String> {
    let resolved = std::fs::canonicalize(device).ok()?;
    let name = resolved.file_name()?.to_str()?;
    let subsystem = std::fs::read_link(device.join("subsystem")).ok()?;
    let subsystem = subsystem.file_name()?.to_str()?;
    Some(format!("{subsystem}-{name}"))
}

/// Connected connectors of `card`, named like outputs: `card1-DP-1` is `DP-1`.
fn connected_outputs_in(sysfs_drm: &Path, card: &str) -> Vec<String> {
    let prefix = format!("{card}-");
    let mut outputs = std::fs::read_dir(sysfs_drm)
        .ok()
        .into_iter()
        .flat_map(|entries| entries.filter_map(Result::ok))
        .filter_map(|entry| {
            let name = entry
                .file_name()
                .to_str()?
                .strip_prefix(&prefix)?
                .to_owned();
            let status = std::fs::read_to_string(entry.path().join("status")).ok()?;
            (status.trim() == "connected").then_some(name)
        })
        .collect::<Vec<_>>();
    outputs.sort();
    outputs
}

fn hardware_database_name(device: &Path) -> Option<String> {
    let device = smithay::reexports::udev::Device::from_syspath(device).ok()?;
    let name = device.property_value("ID_MODEL_FROM_DATABASE")?;
    Some(name.to_str()?.to_owned())
}

/// Sorts by [`GpuSnapshot::id`], so the list keeps its order across boots.
pub fn sort_gpus(gpus: &mut [GpuSnapshot]) {
    gpus.sort_by(|a, b| a.id.cmp(&b.id));
}

impl crate::state::ShojiWM {
    /// Describe the seat's GPUs again (a GPU or a display came or went) and
    /// tell the config when that changed anything.
    pub fn refresh_gpus(&mut self) {
        let mut gpus = self
            .tty_gpu_paths
            .iter()
            .map(|(node, path)| {
                let mut gpu = describe_gpu(path);
                gpu.is_render = self.tty_render_node == Some(*node);
                gpu.in_use = self.tty_backends.contains_key(node);
                gpu
            })
            .collect::<Vec<_>>();
        sort_gpus(&mut gpus);
        if gpus != self.gpus {
            tracing::info!(?gpus, "gpus changed");
            self.gpus = gpus;
        }
        self.sync_gpu_state();
    }

    /// Send the GPUs to the config when they differ from the last ones sent
    /// (also after a reload, which forgets what was sent).
    pub fn sync_gpu_state(&mut self) {
        if self.config_runtime.sync_gpu_state(&self.gpus) {
            self.runtime_scheduler_enabled = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeSysfs {
        root: PathBuf,
    }

    impl FakeSysfs {
        fn new(test: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("shojiwm-gpu-{test}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("class/drm")).unwrap();
            Self { root }
        }

        fn drm(&self) -> PathBuf {
            self.root.join("class/drm")
        }

        /// A PCI GPU at `address`, its card linked into `class/drm`.
        fn pci_gpu(&self, card: &str, address: &str, files: &[(&str, &str)], driver: &str) {
            let device = self.root.join("devices/pci0000:00").join(address);
            std::fs::create_dir_all(device.join("drm").join(card)).unwrap();
            for (name, value) in files {
                std::fs::write(device.join(name), value).unwrap();
            }
            std::fs::create_dir_all(self.root.join("bus/pci/drivers").join(driver)).unwrap();
            std::os::unix::fs::symlink(
                self.root.join("bus/pci/drivers").join(driver),
                device.join("driver"),
            )
            .unwrap();
            std::fs::create_dir_all(self.root.join("bus/pci")).unwrap();
            std::os::unix::fs::symlink(self.root.join("bus/pci"), device.join("subsystem"))
                .unwrap();
            let card_dir = self.drm().join(card);
            std::fs::create_dir_all(&card_dir).unwrap();
            std::os::unix::fs::symlink(&device, card_dir.join("device")).unwrap();
        }

        fn render_node(&self, address: &str, node: &str) {
            let drm = self
                .root
                .join("devices/pci0000:00")
                .join(address)
                .join("drm");
            std::fs::create_dir_all(drm.join(node)).unwrap();
        }

        fn connector(&self, name: &str, status: &str) {
            let dir = self.drm().join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("status"), format!("{status}\n")).unwrap();
        }
    }

    impl Drop for FakeSysfs {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn a_pci_gpu_is_described_from_sysfs() {
        let sysfs = FakeSysfs::new("describe");
        sysfs.pci_gpu(
            "card1",
            "0000:03:00.0",
            &[
                ("vendor", "0x1002\n"),
                ("device", "0x747e\n"),
                ("boot_vga", "0\n"),
                ("removable", "removable\n"),
            ],
            "amdgpu",
        );
        sysfs.render_node("0000:03:00.0", "renderD129");
        sysfs.connector("card1-DP-2", "connected");
        sysfs.connector("card1-DP-1", "connected");
        sysfs.connector("card1-HDMI-A-1", "disconnected");
        // Another card's connector must not be counted.
        sysfs.connector("card10-DP-1", "connected");

        let gpu = describe_gpu_in(&sysfs.drm(), "card1");
        assert_eq!(
            gpu,
            GpuSnapshot {
                id: "pci-0000:03:00.0".into(),
                dynamic_card_id: "card1".into(),
                render_node: Some("/dev/dri/renderD129".into()),
                vendor: GpuVendor::Amd,
                vendor_id: 0x1002,
                device_id: 0x747e,
                driver: Some("amdgpu".into()),
                name: None,
                boot_vga: false,
                removable: true,
                outputs: vec!["DP-1".into(), "DP-2".into()],
                is_render: false,
                in_use: false,
            }
        );
    }

    #[test]
    fn a_gpu_without_device_files_falls_back_to_its_card() {
        let sysfs = FakeSysfs::new("bare");
        std::fs::create_dir_all(sysfs.drm().join("card0")).unwrap();
        let gpu = describe_gpu_in(&sysfs.drm(), "card0");
        assert_eq!(gpu.id, "card0");
        assert_eq!(gpu.vendor, GpuVendor::Other);
        assert_eq!(gpu.vendor_id, 0);
        assert_eq!(gpu.driver, None);
        assert!(!gpu.boot_vga && !gpu.removable);
        assert!(gpu.outputs.is_empty());
    }

    fn gpu(id: &str, card: &str, driver: &str, vendor_id: u32, device_id: u32) -> GpuSnapshot {
        GpuSnapshot {
            id: id.into(),
            dynamic_card_id: card.into(),
            render_node: Some("/dev/dri/renderD128".into()),
            vendor: GpuVendor::from_pci_id(vendor_id),
            vendor_id,
            device_id,
            driver: Some(driver.into()),
            name: None,
            boot_vga: false,
            removable: false,
            outputs: Vec::new(),
            is_render: false,
            in_use: true,
        }
    }

    #[test]
    fn a_gpu_is_matched_by_any_of_its_names() {
        let gpu = gpu("pci-0000:03:00.0", "card1", "amdgpu", 0x1002, 0x747e);
        for choice in [
            "pci-0000:03:00.0",
            " pci-0000:03:00.0 ",
            "pci-0000_03_00_0",
            "card1",
            "/dev/dri/card1",
            "/dev/dri/renderD128",
        ] {
            assert!(gpu.matches(choice), "{choice:?} should match");
        }
        for choice in ["", "card10", "pci-0000:03:00.1", "auto"] {
            assert!(!gpu.matches(choice), "{choice:?} should not match");
        }
    }

    #[test]
    fn offload_env_follows_the_driver() {
        let amd = gpu("pci-0000:03:00.0", "card1", "amdgpu", 0x1002, 0x747e);
        assert_eq!(
            offload_env(&amd),
            vec![
                ("DRI_PRIME".to_owned(), "pci-0000_03_00_0".to_owned()),
                ("MESA_VK_DEVICE_SELECT".to_owned(), "1002:747e".to_owned()),
            ]
        );
        let nvidia = gpu("pci-0000:01:00.0", "card0", "nvidia", 0x10de, 0x2684);
        assert_eq!(
            offload_env(&nvidia)
                .into_iter()
                .map(|(key, _)| key)
                .collect::<Vec<_>>(),
            [
                "__NV_PRIME_RENDER_OFFLOAD",
                "__GLX_VENDOR_LIBRARY_NAME",
                "__VK_LAYER_NV_optimus"
            ]
        );
        // nouveau is a Mesa driver.
        let nouveau = gpu("pci-0000:01:00.0", "card0", "nouveau", 0x10de, 0x2684);
        assert_eq!(offload_env(&nouveau)[0].0, "DRI_PRIME");
        // Not on PCI: no Vulkan device id to give.
        let platform = gpu("platform-gpu", "card0", "panfrost", 0, 0);
        assert_eq!(
            offload_env(&platform),
            vec![("DRI_PRIME".to_owned(), "platform-gpu".to_owned())]
        );
    }
}
