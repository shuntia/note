# The module end to end: the hardened unit starts, answers, and a user made
# with note-ctl can sign in.
self:
{ pkgs, ... }:

{
  name = "note-module";

  nodes.machine = {
    imports = [ self.nixosModules.default ];
    environment.systemPackages = [ pkgs.curl ];
    services.note = {
      enable = true;
      credentials.admin_totp = pkgs.writeText "admin_totp" "JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP";
      settings.public_base_url = "http://127.0.0.1:3271";
    };
  };

  testScript = ''
    machine.wait_for_unit("note.service")
    machine.wait_for_open_port(3271)
    machine.succeed("curl -fsS http://127.0.0.1:3271/healthz | grep -qx ok")
    machine.fail("journalctl -u note.service | grep -q admin_totp")

    machine.succeed("note-ctl create-user alice pw")
    machine.succeed(
      "curl -fsS -D - -o /dev/null -H 'content-type: application/json' "
      "-d '{\"username\":\"alice\",\"password\":\"pw\"}' "
      "http://127.0.0.1:3271/api/login | grep -qi '^set-cookie:'"
    )
    machine.fail(
      "curl -fsS -H 'content-type: application/json' "
      "-d '{\"username\":\"alice\",\"password\":\"wrong\"}' "
      "http://127.0.0.1:3271/api/login"
    )

    machine.succeed("test -f /var/lib/note/data/note.db")
    machine.succeed("systemctl restart note.service")
    machine.wait_for_open_port(3271)
    machine.succeed("curl -fsS http://127.0.0.1:3271/healthz")
  '';
}
