# A new package: the hash is still lib.fakeHash.
{ stdenv, fetchurl }:
stdenv.mkDerivation (finalAttrs: {
  pname = "hello-copy";
  version = "2.12.2";
  src = fetchurl {
    url = "mirror://gnu/hello/hello-${finalAttrs.version}.tar.gz";
    hash = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
  };
})
