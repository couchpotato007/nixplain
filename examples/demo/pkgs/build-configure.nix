# meson needs pkg-config to find libxml2, and libxml2 is missing too.
{ stdenv, meson, ninja }:
stdenv.mkDerivation {
  pname = "libfoo";
  version = "1.4.2";
  src = ./src/meson-lib;
  nativeBuildInputs = [ meson ninja ];
}
