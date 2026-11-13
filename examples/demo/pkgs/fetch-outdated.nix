# The version was bumped from 2.12.1, but the hash was not updated.
{ stdenv, fetchurl }:
stdenv.mkDerivation (finalAttrs: {
  pname = "hello-copy";
  version = "2.12.2";
  src = fetchurl {
    url = "mirror://gnu/hello/hello-${finalAttrs.version}.tar.gz";
    hash = "sha256-jZkUKv2SV28wsM18tCqNxoCZmLxdYH2Idh9RLibH2yA=";
  };
})
