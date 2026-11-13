# Old-style `sha256 = "<base32>"`: the version was bumped from 2.7.5, the
# hash was not. nixplain finds the hash in any encoding and --fix keeps it.
{ stdenv, fetchurl }:
stdenv.mkDerivation rec {
  pname = "patch-copy";
  version = "2.7.6";
  src = fetchurl {
    url = "mirror://gnu/patch/patch-${version}.tar.xz";
    sha256 = "16d2r9kpivaak948mxzc0bai45mqfw73m113wrkmbffnalv1b5gx";
  };
}
