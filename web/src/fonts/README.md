# Self-hosted fonts

woff2 files served from the bundle; no external font requests. All are derived
from OFL-licensed upstream fonts; the licence text must ship with them.

## Files

| File | Family | Notes |
| --- | --- | --- |
| `bricolage-grotesque.woff2` | Bricolage Grotesque | Variable, live axes `opsz` 12–96 and `wght` 200–800. Google Fonts' `latin` slice (U+0000-00FF and common punctuation). |
| `atkinson-400.woff2` | Atkinson Hyperlegible Regular | static |
| `atkinson-700.woff2` | Atkinson Hyperlegible Bold | static |
| `atkinson-400-italic.woff2` | Atkinson Hyperlegible Italic | static |

Bricolage is display only — the wordmark, view titles and card titles. Atkinson
sets everything else and keeps `tnum` in GSUB in all three weights.

## Upstream sources

- Bricolage Grotesque: the `latin` woff2 from
  `https://fonts.googleapis.com/css2?family=Bricolage+Grotesque:opsz,wght@12..96,300..700&display=swap`
  (upstream project: https://github.com/ateliertriay/bricolage)
- https://raw.githubusercontent.com/google/fonts/main/ofl/atkinsonhyperlegible/AtkinsonHyperlegible-Regular.ttf
- https://raw.githubusercontent.com/google/fonts/main/ofl/atkinsonhyperlegible/AtkinsonHyperlegible-Bold.ttf
- https://raw.githubusercontent.com/google/fonts/main/ofl/atkinsonhyperlegible/AtkinsonHyperlegible-Italic.ttf

## Regenerating

Bricolage: request the css2 URL above with a desktop UA and download the woff2
whose `unicode-range` is the plain Latin block (`U+0000-00FF`).

Atkinson requires `fonttools` and `brotli`:

```sh
U='U+0020-007E,U+00A0,U+00A7,U+00B0,U+00B1,U+00B7,U+00D7,U+00C0-00FF,U+0100-017F,U+2010-2015,U+2018,U+2019,U+201C,U+201D,U+2026,U+2039,U+203A,U+2190,U+2212,U+2248,U+2260,U+2264,U+2265,U+22EF,U+25B4,U+25B6,U+25B8,U+25BE,U+2600,U+2713,U+2715,U+FE0E,U+FF0B'
LF='--layout-features+=tnum,kern,liga,clig,onum,lnum,ss01'

pyftsubset AtkinsonHyperlegible-Regular.ttf --unicodes="$U" $LF \
  --flavor=woff2 --output-file=atkinson-400.woff2
pyftsubset AtkinsonHyperlegible-Bold.ttf --unicodes="$U" $LF \
  --flavor=woff2 --output-file=atkinson-700.woff2
pyftsubset AtkinsonHyperlegible-Italic.ttf --unicodes="$U" $LF \
  --flavor=woff2 --output-file=atkinson-400-italic.woff2
```

## Licence

Both families are SIL Open Font License 1.1: `BricolageGrotesque-OFL.txt`,
`AtkinsonHyperlegible-OFL.txt`. Keep these files alongside the woff2s in any
distribution.
