# Third-party notices and credits

The [MIT licence](LICENSE) covers Sequoia Map's original software. Third-party
imagery, fonts and data are not relicensed by that grant.

## Map imagery and data

- **[Wynncraft](https://wynncraft.com/)**: the game world and public territory,
  guild and leaderboard data. See the [API documentation](https://docs.wynncraft.com/)
  and [API terms](https://wynncraft.com/api-terms).
- **[Wynntils](https://wynntils.com/)**: the upstream map-tile catalogue and
  Athena territory and guild-colour data. The
  [map catalogue](https://github.com/Wynntils/Static-Storage/blob/main/Reference/maps.json)
  is also the upstream source used by Wynnmap's tile integration.
- **[Zatzou / Wynnmap](https://github.com/Zatzou/wynnmap)**: map-coordinate
  integration, map inspiration and the
  [territory-extra dataset](https://gist.github.com/Zatzou/14c82f2df0eb4093dfa1d543b78a73a8).

Local map imagery has been cropped and converted to WebP, with a lower-resolution
set for distant views.

## Fonts

### Inter

Copyright (c) 2016 The Inter Project Authors (https://github.com/rsms/inter).

Inter is distributed under the SIL Open Font License, Version 1.1. The complete
[copyright and licence notice](client/public/fonts/OFL-Inter.txt) accompanies the
font in the public font directory and must remain with redistributed copies.

### Minecraft font family

The Minecraft font family is by **Jacob Debono / JDGraphics Fonts**. The
[publisher's FontSpace listing](https://www.fontspace.com/minecraft-font-f28180)
and family download identify its licence as **Public Domain**.

The bundled OpenType files are used by the map's glyph atlas. The WOFF2 files
are re-hinted versions used by the site interface. Source and conversion notes
are retained in [fonts/info.txt](client/public/fonts/info.txt).
