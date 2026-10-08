# programs.note-desktop for NixOS. On a host that also runs services.note,
# `url` defaults to that server's public_base_url.
self:
{ config, lib, pkgs, ... }:

let
  cfg = config.programs.note-desktop;
  server = config.services.note or { enable = false; };
in
{
  options.programs.note-desktop = import ./desktop-options.nix self {
    inherit lib pkgs;
    urlDefault = if server.enable then server.settings.public_base_url else null;
    urlDefaultText = lib.literalExpression
      "if config.services.note.enable then config.services.note.settings.public_base_url else null";
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ (cfg.package.override { inherit (cfg) url; }) ];
  };
}
