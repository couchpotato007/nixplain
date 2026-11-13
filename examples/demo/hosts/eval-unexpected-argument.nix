let
  mkUser = { name, uid }: {
    ${name} = {
      isNormalUser = true;
      inherit uid;
    };
  };
in
{
  users.users = mkUser {
    name = "bob";
    uid = 1001;
    shell = "/bin/zsh";
  };
}
