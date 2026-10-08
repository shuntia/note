# programs.note-desktop, shared by the NixOS and Home Manager modules.
self:
{ lib, pkgs, urlDefault ? null, urlDefaultText ? null }:

{
  enable = lib.mkEnableOption "the Note desktop app";

  package = lib.mkOption {
    type = lib.types.package;
    default = self.packages.${pkgs.stdenv.hostPlatform.system}.note-desktop;
    defaultText = lib.literalExpression "note.packages.\${system}.note-desktop";
    description = "The desktop package; `url` is applied to it with `.override`.";
  };

  url = lib.mkOption {
    type = lib.types.nullOr lib.types.str;
    default = urlDefault;
    defaultText = if urlDefaultText == null then lib.literalExpression "null" else urlDefaultText;
    example = "https://note.example.com";
    description = ''
      The Note server the app opens until the user picks another with
      "Change server…". Null leaves it to the first-run prompt.
    '';
  };
}
