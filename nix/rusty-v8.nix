{
  fetchurl,
  stdenv,
}:

let
  version = "142.2.0";
  system = stdenv.hostPlatform.system;
  targets = {
    x86_64-linux = "x86_64-unknown-linux-gnu";
    aarch64-linux = "aarch64-unknown-linux-gnu";
  };
  hashes = {
    x86_64-linux = "sha256-xHmofo8wTNg88/TuC2pX2OHDRYtHncoSvSBnTV65o+0=";
    aarch64-linux = "sha256-24q6wX8RTRX1tMGqgcz9/wN3Y+hWxM2SEuVrYhECyS8=";
  };
  target =
    targets.${system}
      or (throw "ShojiWM's embedded Deno runtime does not support ${system}");
in
fetchurl {
  name = "rusty-v8-${version}-${target}.a.gz";
  url = "https://github.com/denoland/rusty_v8/releases/download/v${version}/librusty_v8_release_${target}.a.gz";
  hash = hashes.${system};
}
