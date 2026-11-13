let
  mkUser = { name, uid }: {
    ${name} = {
      isNormalUser = true;
      inherit uid;
    };
  };
in
{
  users.users = mkUser { name = "bob"; };
}
