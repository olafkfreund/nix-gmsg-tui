{
  stdenvNoCC,
  libnotify,
  qt6,
}:

stdenvNoCC.mkDerivation {
  pname = "gmessages-plugin";
  version = (builtins.fromJSON (builtins.readFile ../plugin/manifest.json)).version;

  src = ../plugin;

  nativeInstallCheckInputs = [ qt6.qtdeclarative ];
  dontWrapQtApps = true;

  installPhase = ''
    runHook preInstall
    cp -r . $out
    substituteInPlace $out/Service.qml --replace-fail @notifySend@ ${libnotify}/bin/notify-send
    runHook postInstall
  '';

  doInstallCheck = true;
  # Parse only: the qs.* modules exist only inside the Omarchy shell.
  installCheckPhase = ''
    runHook preInstallCheck
    qmllint --import disable --unresolved-type disable --missing-type disable \
      --missing-property disable --unqualified disable $out/*.qml
    ! grep -r @notifySend@ $out
    runHook postInstallCheck
  '';
}
