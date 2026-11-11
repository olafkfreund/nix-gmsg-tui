{
  lib,
  rustPlatform,
  pkg-config,
  wrapGAppsHook3,
  perl,
  openssl,
  dbus,
  gtk3,
  webkitgtk_4_1,
  libsoup_3,
  glib-networking,
}:

rustPlatform.buildRustPackage {
  pname = "gmessages-tui";
  version = "1.3.6";

  src = lib.cleanSourceWith {
    src = ../.;
    filter =
      path: type:
      let
        relative = lib.removePrefix "${toString ../.}/" (toString path);
      in
      relative != "flake.nix"
      && relative != "flake.lock"
      && !(lib.any (dir: relative == dir || lib.hasPrefix "${dir}/" relative) [
        ".git"
        "nix"
        "intent"
        "spec"
        "plan"
        "target"
        "result"
      ]);
  };

  cargoLock = {
    lockFile = ../Cargo.lock;
  };

  nativeBuildInputs = [
    pkg-config
    wrapGAppsHook3
    perl
  ];
  buildInputs = [
    openssl
    dbus
    gtk3
    webkitgtk_4_1
    libsoup_3
    glib-networking
  ];
  cargoTestFlags = [ "--workspace" ];

  # nixpkgs' cargoUseSystemLibs exports LIBSQLITE3_SYS_USE_PKG_CONFIG=1, which overrides
  # rusqlite's bundled-sqlcipher and links a system SQLCipher against the bundled bindings.
  # Build the bundled SQLCipher, as upstream and the distro packages do.
  preBuild = ''
    export LIBSQLITE3_SYS_USE_PKG_CONFIG=0
  '';

  meta = {
    license = lib.licenses.agpl3Only;
    mainProgram = "gmessages-tui";
  };
}
