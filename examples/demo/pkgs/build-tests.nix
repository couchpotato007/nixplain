# The package builds, but its test suite fails.
{ stdenv }:
stdenv.mkDerivation {
  pname = "calc";
  version = "3.1";
  dontUnpack = true;
  buildPhase = "echo 'echo 4' > calc && chmod +x calc";
  doCheck = true;
  checkPhase = ''
    runHook preCheck
    echo "test_add ... ok"
    echo "test_div_by_zero ... FAILED"
    echo "2 passed, 1 failed"
    exit 1
  '';
  installPhase = "install -Dm755 calc $out/bin/calc";
}
