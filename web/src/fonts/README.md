# Self-hosted fonts

Subsetted woff2 files. All are derived works of OFL-licensed upstream fonts; the
licence text must ship with them.

## Files

| File | Family | Notes |
| --- | --- | --- |
| `fraunces-subset.woff2` | Fraunces | Variable. Live axes: `opsz` 9–144, `wght` 420–560 (default 560). `SOFT`/`WONK` pinned to 0. |
| `atkinson-400.woff2` | Atkinson Hyperlegible Regular | static |
| `atkinson-700.woff2` | Atkinson Hyperlegible Bold | static |
| `atkinson-400-italic.woff2` | Atkinson Hyperlegible Italic | static |

Fraunces has no `tnum` feature and proportional digits, so
`font-variant-numeric: tabular-nums` has no effect on it. Atkinson keeps `tnum`
in GSUB in all three weights.

## Upstream sources

- https://raw.githubusercontent.com/google/fonts/main/ofl/fraunces/Fraunces%5BSOFT%2CWONK%2Copsz%2Cwght%5D.ttf
- https://raw.githubusercontent.com/google/fonts/main/ofl/atkinsonhyperlegible/AtkinsonHyperlegible-Regular.ttf
- https://raw.githubusercontent.com/google/fonts/main/ofl/atkinsonhyperlegible/AtkinsonHyperlegible-Bold.ttf
- https://raw.githubusercontent.com/google/fonts/main/ofl/atkinsonhyperlegible/AtkinsonHyperlegible-Italic.ttf

## Regenerating

Requires `fonttools` and `brotli`.

```sh
U='U+0020-007E,U+00A0,U+00A7,U+00B0,U+00B1,U+00B7,U+00D7,U+00C0-00FF,U+0100-017F,U+2010-2015,U+2018,U+2019,U+201C,U+201D,U+2026,U+2039,U+203A,U+2190,U+2212,U+2248,U+2260,U+2264,U+2265,U+22EF,U+25B4,U+25B6,U+25B8,U+25BE,U+2600,U+2713,U+2715,U+FE0E,U+FF0B'
LF='--layout-features+=tnum,kern,liga,clig,onum,lnum,ss01'

fonttools varLib.instancer 'Fraunces[SOFT,WONK,opsz,wght].ttf' \
  SOFT=0 WONK=0 wght=420:560 -o Fraunces-partial.ttf
pyftsubset Fraunces-partial.ttf --unicodes="$U" $LF \
  --flavor=woff2 --output-file=fraunces-subset.woff2

pyftsubset AtkinsonHyperlegible-Regular.ttf --unicodes="$U" $LF \
  --flavor=woff2 --output-file=atkinson-400.woff2
pyftsubset AtkinsonHyperlegible-Bold.ttf --unicodes="$U" $LF \
  --flavor=woff2 --output-file=atkinson-700.woff2
pyftsubset AtkinsonHyperlegible-Italic.ttf --unicodes="$U" $LF \
  --flavor=woff2 --output-file=atkinson-400-italic.woff2
```

## Licence

Both families are SIL Open Font License 1.1: `Fraunces-OFL.txt`,
`AtkinsonHyperlegible-OFL.txt`. Keep these files alongside the woff2s in any
distribution.
