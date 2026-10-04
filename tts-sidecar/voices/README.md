# Reference voices

Chatterbox clones a voice from a short reference clip. Every `<id>.wav` or
`<id>.flac` in the voices directory becomes voice `<id>`; `voices.json` maps
ids to the labels shown in the voice sheet. The built-in voice (`default`)
ships with the model.

The clips are not stored here: the Nix package fetches them (see
`flake.nix`) from [Voice-Zero](https://github.com/OwenTyme/voice-zero) at
commit `490cfbee850a6d409076f477c766f567000a79b6`. Voice-Zero's `voices/`
directory is CC0 (its `LICENSE.md`: "Unless otherwise noted, all files in
this repository are under the CC0 license"; its README: "This directory will
*always* hold CC0-licensed samples"). The clips are cleaned excerpts of
LibriVox recordings, which LibriVox releases into the public domain.

| id | Voice-Zero file | Reader | Accent | LibriVox source |
| --- | --- | --- | --- | --- |
| `carrie` | `voices/carrie_mae_streb.flac` | [Carrie Mae Streb](https://librivox.org/reader/20156) | American | LibriVox 20th Anniversary Collection, track 20 |
| `caro` | `voices/caro_davy.flac` | [Caro Davy](https://librivox.org/reader/10955) | English | LibriVox 20th Anniversary Collection, track 25 |
| `bill` | `voices/bill_boerst.flac` | [Bill Boerst](https://librivox.org/reader/4788) | American | Short Story Collection Vol. 042, track 9 |
| `stuart` | `voices/stuart_bell.flac` | [Stuart Bell](https://librivox.org/reader/1870) | English, southern | Celebration of Dialects and Accents, Vol 1, track 26 |
