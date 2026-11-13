# `libfoo` is not in nixpkgs, so callPackage cannot fill it in.
{ stdenv, libfoo }:
stdenv.mkDerivation {
  pname = "uses-libfoo";
  version = "1.0";
  buildInputs = [ libfoo ];
}
