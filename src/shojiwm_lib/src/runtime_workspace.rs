#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeWorkspaceConfigUpdate {
    #[serde(default)]
    pub groups: Vec<RuntimeWorkspaceGroupConfig>,
    /// KDE's virtual desktops (`org_kde_plasma_virtual_desktop_management`,
    /// for plasmashell's Pager), if the config maps workspaces to them.
    #[serde(default)]
    pub virtual_desktops: Option<RuntimeVirtualDesktopConfig>,
    /// The config listens to `workspace.event.onCreate`, so groups advertise
    /// ext-workspace's `create_workspace`.
    #[serde(default)]
    pub can_create: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeVirtualDesktopConfig {
    /// In order: a desktop's position is its index here.
    #[serde(default)]
    pub desktops: Vec<RuntimeVirtualDesktopEntry>,
    /// How many rows the Pager lays the desktops out in (default 1).
    #[serde(default)]
    pub rows: Option<u32>,
    /// Windows that are really minimized. A window on an inactive desktop is
    /// hidden too, but must not look minimized to the taskbar.
    #[serde(default)]
    pub minimized_window_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeVirtualDesktopEntry {
    /// Activating the desktop activates the workspace with this id.
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub active: bool,
    /// The windows on this desktop.
    #[serde(default)]
    pub window_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeWorkspaceGroupConfig {
    pub id: String,
    #[serde(default)]
    pub outputs: Vec<String>,
    #[serde(default)]
    pub workspaces: Vec<RuntimeWorkspaceEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeWorkspaceEntry {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub coordinates: Vec<u32>,
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub urgent: bool,
    #[serde(default)]
    pub hidden: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeWorkspaceActivateRequestSnapshot {
    pub workspace_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
    /// Move this window to the workspace instead of switching to it. The
    /// runtimes deliver such a request to `onMoveWindow`, not `onActivate`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_id: Option<String>,
    /// Create a workspace with this name in `group_id` instead; `workspace_id`
    /// is empty. The runtimes deliver such a request to `onCreate`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub create_name: Option<String>,
}

/// A client (a bar) asked to create a workspace (`workspace.event.onCreate`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeWorkspaceCreateEvent {
    pub group_id: Option<String>,
    pub name: String,
}

/// A client (KDE's Pager or taskbar) asked to move a window to a workspace
/// (`workspace.event.onMoveWindow`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeWorkspaceMoveWindowEvent {
    pub workspace_id: String,
    pub window_id: String,
}
