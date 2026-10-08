# programs.note-desktop for Home Manager.
self:
{ config, lib, pkgs, ... }:

let
  cfg = config.programs.note-desktop;
in
{
  options.programs.note-desktop = import ./desktop-options.nix self { inherit lib pkgs; };

  config = lib.mkIf cfg.enable {
    home.packages = [ (cfg.package.override { inherit (cfg) url; }) ];
  };
}
