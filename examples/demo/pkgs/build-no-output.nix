# Builds fine, but the meson project never installs the executable.
{ stdenv, meson, ninja }:
stdenv.mkDerivation {
  pname = "tool";
  version = "0.3.0";
  src = ./src/meson-noinstall;
  nativeBuildInputs = [ meson ninja ];
}
