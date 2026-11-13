# Fails without any recognisable error message.
{ stdenv }:
stdenv.mkDerivation {
  pname = "mystery";
  version = "0.0.1";
  dontUnpack = true;
  buildPhase = ''
    echo "generating files..."
    echo "step 2/3"
    false
  '';
}
