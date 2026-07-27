{
  description = "ShojiWM, a TypeScript-configured Wayland compositor";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs =
    { self, nixpkgs }:
    let
      lib = nixpkgs.lib;
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = lib.genAttrs systems;
      pkgsFor = system: import nixpkgs { inherit system; };
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
          libgbm = pkgs.libgbm or pkgs.mesa;
          xwayland = pkgs.xwayland or (pkgs.xorg.xwayland or null);
          xwaylandSatellite = pkgs.xwayland-satellite or null;
        in
        rec {
          shojiwm = pkgs.callPackage ./nix/package.nix {
            inherit libgbm xwayland xwaylandSatellite;
          };
          default = shojiwm;
        }
      );

      apps = forAllSystems (
        system:
        let
          package = self.packages.${system}.default;
        in
        {
          default = {
            type = "app";
            program = "${package}/bin/shoji_wm";
          };
          init-config = {
            type = "app";
            program = "${package}/bin/shojiwm-init-config";
          };
        }
      );

      devShells = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
          libgbm = pkgs.libgbm or pkgs.mesa;
          xwayland = pkgs.xwayland or (pkgs.xorg.xwayland or null);
          xwaylandSatellite = pkgs.xwayland-satellite or null;
          rustyV8Archive = pkgs.callPackage ./nix/rusty-v8.nix { };
          runtimeLibraryPath = lib.makeLibraryPath (
            with pkgs;
            [
              wayland
              libxkbcommon
              systemd
              libinput
              mesa
              libglvnd
              libgbm
              pixman
              seatd
              pipewire
              libdrm
            ]
          );
          gbmBackendsPath = lib.makeSearchPath "lib/gbm" [
            pkgs.mesa
          ];
          driDriversPath = lib.makeSearchPath "lib/dri" [
            pkgs.mesa
          ];
          eglVendorLibraryDirs = lib.makeSearchPath "share/glvnd/egl_vendor.d" [
            pkgs.mesa
          ];
        in
        {
          default = pkgs.mkShell {
            packages =
              with pkgs;
              [
                cargo
                clang
                rustc
                rustfmt
                clippy
                nodejs_22
                pkg-config
                wayland
                wayland-protocols
                libxkbcommon
                systemd
                libinput
                mesa
                libglvnd
                libgbm
                pixman
                seatd
                pipewire
                libdrm
                dbus
              ]
              ++ lib.optional (xwayland != null) xwayland
              ++ lib.optional (xwaylandSatellite != null) xwaylandSatellite;

            SHOJI_XWAYLAND_SATELLITE_PATH = lib.optionalString (
              xwaylandSatellite != null
            ) "${xwaylandSatellite}/bin/xwayland-satellite";

            LD_LIBRARY_PATH = runtimeLibraryPath;
            GBM_BACKENDS_PATH = gbmBackendsPath;
            LIBGL_DRIVERS_PATH = driDriversPath;
            __EGL_VENDOR_LIBRARY_DIRS = eglVendorLibraryDirs;
            LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib or pkgs.llvmPackages.libclang}/lib";
            RUSTY_V8_ARCHIVE = rustyV8Archive;

            shellHook = ''
              echo "ShojiWM development shell"
              echo "Run: cargo run --release -p shoji_wm -- --dev"
              echo "Node/npm is only needed for TypeScript checks and documentation tools."
            '';
          };
        }
      );

      nixosModules.default = import ./nix/nixos-module.nix { inherit self; };
    };
}
