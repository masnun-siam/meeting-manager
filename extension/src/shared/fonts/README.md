# Bundled fonts

Loaded by `@font-face` in `../tokens.css`. Both are under the SIL Open Font License 1.1 (texts beside them) and declare no Reserved Font Name.

| File | Source | Made with |
|---|---|---|
| `inter.woff2` | `web/InterVariable.woff2` from [rsms/inter v4.1](https://github.com/rsms/inter/releases/tag/v4.1) | subset to Latin, `opsz` pinned at 14, `wght` 400 to 700 |
| `space-grotesk.woff2` | `woff2/SpaceGrotesk[wght].woff2` from [floriankarsten/space-grotesk 2.0.0](https://github.com/floriankarsten/space-grotesk/releases/tag/2.0.0) | subset to Latin, `wght` 400 to 700 |

To remake them (a dev-time tool, not a project dependency), with `U` the `unicode-range` in `tokens.css` without spaces:

```sh
uvx --from 'fonttools[woff]==4.60.1' pyftsubset InterVariable.woff2 --unicodes="$U" --layout-features='*' --flavor=woff2 --output-file=inter-subset.woff2
uvx --from 'fonttools[woff]==4.60.1' fonttools varLib.instancer inter-subset.woff2 opsz=14 wght=400:700 -o inter.woff2
uvx --from 'fonttools[woff]==4.60.1' pyftsubset 'SpaceGrotesk[wght].woff2' --unicodes="$U" --layout-features='*' --flavor=woff2 --output-file=sg-subset.woff2
uvx --from 'fonttools[woff]==4.60.1' fonttools varLib.instancer sg-subset.woff2 wght=400:700 -o space-grotesk.woff2
```
