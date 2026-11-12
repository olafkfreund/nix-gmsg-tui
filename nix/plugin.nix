{
  lib,
  stdenvNoCC,
  jq,
  libnotify,
  qt6,
}:

let
  src = ../plugin;
  # Qt's QML disk cache keys a file on its URL and mtime, and every store file has the
  # same mtime: under one URL an updated plugin would keep running the old compiled
  # QML. A directory named after the source hash gives each change new URLs.
  tag = "v-" + builtins.substring 11 12 "${src}";
in
stdenvNoCC.mkDerivation {
  pname = "gmessages-plugin";
  version = (lib.importJSON ../plugin/manifest.json).version;

  inherit src;

  nativeBuildInputs = [ jq ];
  nativeInstallCheckInputs = [ qt6.qtdeclarative ];
  dontWrapQtApps = true;

  installPhase = ''
    runHook preInstall
    mkdir -p $out/${tag}
    cp -r . $out/${tag}
    rm $out/${tag}/manifest.json
    jq '.entryPoints |= map_values("${tag}/" + .)' manifest.json > $out/manifest.json
    substituteInPlace $out/${tag}/Service.qml --replace-fail @notifySend@ ${libnotify}/bin/notify-send
    runHook postInstall
  '';

  doInstallCheck = true;
  # Parse only: the qs.* modules exist only inside the Omarchy shell.
  installCheckPhase = ''
    runHook preInstallCheck
    qmllint --import disable --unresolved-type disable --missing-type disable \
      --missing-property disable --unqualified disable $out/${tag}/*.qml
    # Not `! grep`: errexit ignores a negated command.
    if grep -r @notifySend@ $out; then exit 1; fi
    for ep in $(jq -r '.entryPoints[]' $out/manifest.json); do test -f "$out/$ep"; done
    runHook postInstallCheck
  '';
}
