packs a CS 1.6 map and all its custom content into a zip

give it a bsp from a server install and it parses out everything the map references, skips vanilla game files, and zips the rest with correct install paths

```
usage: packmap <mapname.bsp> [mapname2.bsp ...] [folder ...] [--out/-o output_dir]
```

give it a folder and it'll offer to pack every bsp in there

if the bsp isn't inside a server, it writes `mapname_resources.txt` listing everything the map needs instead
