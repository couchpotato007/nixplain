# A new package written the usual way: hash = lib.fakeHash, build once,
# copy the real hash from the error.
{ lib, stdenv, fetchurl }:
stdenv.mkDerivation (finalAttrs: {
  pname = "sed-copy";
  version = "4.9";
  src = fetchurl {
    url = "mirror://gnu/sed/sed-${finalAttrs.version}.tar.xz";
    hash = lib.fakeHash;
  };
})
