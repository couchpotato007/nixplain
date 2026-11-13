# Uses zlib but does not list it in buildInputs.
{ stdenv }:
stdenv.mkDerivation {
  pname = "compress";
  version = "0.1";
  dontUnpack = true;
  buildPhase = ''
    $CC ${./src/compress.c} -lz -o compress
  '';
  installPhase = "install -Dm755 compress $out/bin/compress";
}
