use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use zip::write::{SimpleFileOptions, ZipWriter};
use zip::CompressionMethod;

// ============================================================
// CONSTANTS
// ============================================================

const VANILLA_LIST: &str = include_str!("cs-vanilla-filelist.txt");
const SKY_FACES: &[&str] = &["ft", "bk", "lf", "rt", "up", "dn"];

// ============================================================
// TYPES
// ============================================================

struct BspLump {
    offset: u32,
    length: u32,
}

struct BspInfo {
    has_external_tex: bool,
    entity_text: String,
}

// ============================================================
// MAIN / CLI
// ============================================================

fn usage() -> ! {
    eprintln!("Usage: packmap [-r] [-o <dir>] <mapname.bsp> [mapname2.bsp ...] [folder ...]");
    eprintln!("  -r, --res-only   write only a .res file, even when archiving is available");
    eprintln!("  -o, --out <dir>  write output here instead of next to the packmap binary");
    std::process::exit(1);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        usage();
    }

    let mut out_override: Option<PathBuf> = None;
    let mut res_only = false;
    let mut paths: Vec<String> = Vec::new();

    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-r" | "--res-only" => res_only = true,
            "-o" | "--out" => match it.next() {
                Some(dir) => out_override = Some(PathBuf::from(dir)),
                None => {
                    eprintln!("{} requires a directory argument", arg);
                    usage();
                }
            },
            _ => paths.push(arg),
        }
    }

    if paths.is_empty() {
        usage();
    }

    // Default output location stays next to the binary, so existing behaviour
    // is unchanged when -o is absent.
    let out_dir = match out_override {
        Some(dir) => {
            if !dir.is_dir() {
                eprintln!("Output directory does not exist: {}", dir.display());
                std::process::exit(1);
            }
            dir
        }
        None => std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from(".")),
    };

    let vanilla: HashSet<&str> = VANILLA_LIST.lines().filter(|l| !l.is_empty()).collect();

    let mut all_ok = true;
    for arg in &paths {
        if !process_path(Path::new(arg), &out_dir, &vanilla, res_only) {
            all_ok = false;
        }
    }
    if !all_ok {
        std::process::exit(1);
    }
}

fn process_path(path: &Path, out_dir: &Path, vanilla: &HashSet<&str>, res_only: bool) -> bool {
    if path.is_dir() {
        let entries = match std::fs::read_dir(path) {
            Ok(e) => e,
            Err(e) => { eprintln!("Cannot read directory {}: {}", path.display(), e); return false; }
        };
        let mut bsps: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.eq_ignore_ascii_case("bsp"))
                    .unwrap_or(false)
            })
            .collect();
        bsps.sort();

        println!("Found {} BSP(s) in {}", bsps.len(), path.display());
        if bsps.is_empty() { return true; }

        print!("Pack all of them? [y/N]: ");
        io::stdout().flush().ok();
        let mut answer = String::new();
        io::stdin().read_line(&mut answer).ok();

        if answer.trim().eq_ignore_ascii_case("y") {
            let mut all_ok = true;
            for bsp in &bsps {
                if !pack_bsp(bsp, out_dir, vanilla, res_only) {
                    all_ok = false;
                }
                println!();
            }
            all_ok
        } else {
            println!("Skipped.");
            true
        }
    } else {
        pack_bsp(path, out_dir, vanilla, res_only)
    }
}

fn pack_bsp(bsp_path: &Path, out_dir: &Path, vanilla: &HashSet<&str>, res_only: bool) -> bool {
    if !bsp_path.is_file() {
        eprintln!("BSP not found: {}", bsp_path.display());
        return false;
    }

    let mapname = match bsp_path.file_stem().and_then(|s| s.to_str()) {
        Some(n) => n.to_string(),
        None => { eprintln!("Invalid BSP filename"); return false; }
    };
    let maps_dir = match bsp_path.parent() {
        Some(d) => d,
        None => { eprintln!("Cannot determine BSP directory"); return false; }
    };

    let game_dir = maps_dir.parent();
    let game_dir_name = game_dir
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();
    let server_root = game_dir.and_then(|p| p.parent()).map(|p| p.to_path_buf());

    let loose = server_root.is_none() || !is_known_game_dir(&game_dir_name);

    let search_dirs: Vec<String> = if loose {
        vec![]
    } else {
        let mut dirs = vec![game_dir_name.clone()];
        if let Some(base) = game_dir_name.strip_suffix("_downloads") {
            dirs.push(base.to_string());
        }
        dirs
    };

    println!("Map:  {}", mapname);
    println!("BSP:  {}", bsp_path.display());
    if res_only {
        println!("Mode: resource list only\n");
    } else if loose {
        println!("Mode: loose (resource list only)\n");
    } else {
        println!("Mode: server {} [{}]\n",
            server_root.as_ref().unwrap().display(),
            search_dirs.join(" -> "));
    }

    let bsp_data = match std::fs::read(bsp_path) {
        Ok(d) => d,
        Err(e) => { eprintln!("Cannot read BSP: {}", e); return false; }
    };
    let bsp_info = match parse_bsp(&bsp_data) {
        Ok(i) => i,
        Err(e) => { eprintln!("{}", e); return false; }
    };

    println!("  External textures: {}\n",
        if bsp_info.has_external_tex { "yes" } else { "no (all embedded)" });

    let entities = parse_entities(&bsp_info.entity_text);
    println!("Parsed {} entities\n", entities.len());

    let mut queue: HashMap<String, String> = HashMap::new();
    collect_resources(
        &bsp_data, &bsp_info, &entities, &mapname,
        server_root.as_deref(), &search_dirs, &mut queue,
    );
    println!("Collected {} candidate paths\n", queue.len());

    let result = if loose || res_only {
        write_resource_list(
            &mapname,
            &queue,
            vanilla,
            out_dir,
            if loose { None } else { server_root.as_deref() },
            &search_dirs,
        )
    } else {
        write_zip(
            bsp_path, &mapname, &queue, vanilla,
            server_root.as_deref().unwrap(), &search_dirs, out_dir,
        )
    };

    if let Err(e) = result {
        eprintln!("Output error: {}", e);
        return false;
    }
    true
}

fn is_known_game_dir(name: &str) -> bool {
    matches!(name, "cstrike" | "cstrike_downloads" | "czero" | "czero_downloads")
}

// ============================================================
// BSP PARSING
// ============================================================

fn parse_bsp(data: &[u8]) -> Result<BspInfo, String> {
    if data.len() < 4 {
        return Err("Truncated BSP header".into());
    }
    let ver = u32::from_le_bytes(data[0..4].try_into().unwrap());
    if ver != 30 {
        return Err(format!("Not a GoldSrc BSP (version={}, expected 30)", ver));
    }
    let lumps = read_lump_directory(data)?;
    let has_external_tex = check_external_textures(data, &lumps[2]);
    let entity_text = read_entity_lump(data, &lumps[0])?;
    Ok(BspInfo { has_external_tex, entity_text })
}

fn read_lump_directory(data: &[u8]) -> Result<Vec<BspLump>, String> {
    if data.len() < 124 {
        return Err("BSP too small for lump directory".into());
    }
    let mut lumps = Vec::with_capacity(15);
    for i in 0..15 {
        let base = 4 + i * 8;
        let offset = u32::from_le_bytes(data[base..base + 4].try_into().unwrap());
        let length = u32::from_le_bytes(data[base + 4..base + 8].try_into().unwrap());
        lumps.push(BspLump { offset, length });
    }
    Ok(lumps)
}

fn check_external_textures(data: &[u8], lump: &BspLump) -> bool {
    if lump.length == 0 { return false; }
    let start = lump.offset as usize;
    let end = match start.checked_add(lump.length as usize) {
        Some(e) if e <= data.len() => e,
        _ => return false,
    };
    let tex = &data[start..end];
    if tex.len() < 4 { return false; }

    let ntex = u32::from_le_bytes(tex[0..4].try_into().unwrap()) as usize;
    for i in 0..ntex {
        let oo = 4 + i * 4;
        if oo + 4 > tex.len() { break; }
        let dataofs = i32::from_le_bytes(tex[oo..oo + 4].try_into().unwrap());
        if dataofs == -1 { return true; }
        let dataofs = dataofs as usize;
        if dataofs + 40 > tex.len() { break; }
        let mip0 = u32::from_le_bytes(tex[dataofs + 24..dataofs + 28].try_into().unwrap());
        if mip0 == 0 { return true; }
    }
    false
}

fn read_entity_lump(data: &[u8], lump: &BspLump) -> Result<String, String> {
    let start = lump.offset as usize;
    let end = match start.checked_add(lump.length as usize) {
        Some(e) if e <= data.len() => e,
        _ => return Err("Entity lump extends beyond BSP data".into()),
    };
    let text = String::from_utf8_lossy(&data[start..end]).replace('\0', "");
    Ok(text)
}

// ============================================================
// ENTITY PARSING
// ============================================================

fn parse_entities(text: &str) -> Vec<HashMap<String, String>> {
    let mut entities = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        rest = &rest[open + 1..];
        let close = match rest.find('}') { Some(c) => c, None => break };
        let block = &rest[..close];
        rest = &rest[close + 1..];

        let mut entity: HashMap<String, String> = HashMap::new();
        let mut s = block;
        while let Some(q1) = s.find('"') {
            s = &s[q1 + 1..];
            let q2 = match s.find('"') { Some(i) => i, None => break };
            let key = s[..q2].to_lowercase();
            s = &s[q2 + 1..];

            let q3 = match s.find('"') { Some(i) => i, None => break };
            s = &s[q3 + 1..];
            let q4 = match s.find('"') { Some(i) => i, None => break };
            let val = s[..q4].to_string();
            s = &s[q4 + 1..];

            if !key.is_empty() { entity.insert(key, val); }
        }
        if !entity.is_empty() { entities.push(entity); }
    }
    entities
}

// ============================================================
// RESOURCE COLLECTION
// ============================================================

fn enqueue(path: &str, queue: &mut HashMap<String, String>) {
    let normalized = path.replace('\\', "/");
    let normalized = normalized.trim_start_matches(['/', '\\']);
    if normalized.is_empty() { return; }
    let has_ext = normalized.rfind('.').map(|dot| {
        let ext = &normalized[dot + 1..];
        !ext.is_empty() && ext.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    }).unwrap_or(false);
    if !has_ext { return; }
    let lc = normalized.to_lowercase();
    queue.entry(lc).or_insert_with(|| normalized.to_string());
}

fn normalize_path(val: &str) -> String {
    let s = val.replace('\\', "/");
    let lval = s.to_lowercase();
    // wav paths are relative to sound/; mdl/spr paths are from the game root
    if lval.ends_with(".wav") && !lval.starts_with("sound/") {
        return format!("sound/{}", s);
    }
    if !s.contains('/') {
        if lval.ends_with(".mdl") {
            return format!("models/{}", s);
        }
        if lval.ends_with(".spr") {
            return format!("sprites/{}", s);
        }
    }
    s
}

fn collect_resources(
    bsp_data: &[u8],
    info: &BspInfo,
    entities: &[HashMap<String, String>],
    mapname: &str,
    server_root: Option<&Path>,
    search_dirs: &[String],
    queue: &mut HashMap<String, String>,
) {
    collect_worldspawn(info, entities, server_root, search_dirs, queue);
    collect_entity_resources(entities, queue);
    collect_raw_scan(bsp_data, queue);

    enqueue(&format!("maps/{}.txt", mapname), queue);
    enqueue(&format!("maps/{}_detail.txt", mapname), queue);
    enqueue(&format!("maps/{}.nav", mapname), queue);
    enqueue(&format!("maps/{}.res", mapname), queue);
    enqueue(&format!("overviews/{}.bmp", mapname), queue);
    enqueue(&format!("overviews/{}.txt", mapname), queue);

    if let Some(root) = server_root {
        collect_model_siblings(root, search_dirs, queue);

        let detail_rel = format!("maps/{}_detail.txt", mapname);
        if let Some(detail_full) = find_on_server(&detail_rel, root, search_dirs) {
            collect_detail_textures(&detail_full, queue);
        }
    }
}

// Models can ship unreferenced siblings: "<name>T.mdl" (numtextures == 0)
// and sequence groups "<name>01.mdl"... (numseqgroups > 1).
fn collect_model_siblings(
    server_root: &Path,
    search_dirs: &[String],
    queue: &mut HashMap<String, String>,
) {
    let models: Vec<String> = queue
        .values()
        .filter(|v| v.to_lowercase().ends_with(".mdl"))
        .cloned()
        .collect();

    for rel in models {
        let Some(full) = find_on_server(&rel, server_root, search_dirs) else { continue };
        let Ok(mut f) = File::open(&full) else { continue };

        // studiohdr_t: magic at 0, numseqgroups at 172, numtextures at 180.
        let mut hdr = [0u8; 184];
        if f.read_exact(&mut hdr).is_err() { continue; }
        if &hdr[0..4] != b"IDST" { continue; }
        let numseqgroups = i32::from_le_bytes(hdr[172..176].try_into().unwrap());
        let numtextures = i32::from_le_bytes(hdr[180..184].try_into().unwrap());

        let stem = &rel[..rel.len() - 4];

        if numtextures == 0 {
            enqueue(&format!("{}T.mdl", stem), queue);
        }
        // group 0 is this file
        for g in 1..numseqgroups {
            enqueue(&format!("{}{:02}.mdl", stem, g), queue);
        }
    }
}

fn collect_worldspawn(
    info: &BspInfo,
    entities: &[HashMap<String, String>],
    server_root: Option<&Path>,
    search_dirs: &[String],
    queue: &mut HashMap<String, String>,
) {
    let ws = match entities.iter().find(|e| {
        e.get("classname").map(|v| v == "worldspawn").unwrap_or(false)
    }) {
        Some(e) => e,
        None => return,
    };

    if info.has_external_tex {
        if let Some(wad_list) = ws.get("wad") {
            for raw in wad_list.split(';') {
                let raw = raw.trim().replace('\\', "/");
                let basename = raw.rsplit('/').next().unwrap_or(&raw).to_string();
                if !basename.is_empty() { enqueue(&basename, queue); }
            }
        }
    }

    for key in &["skyname", "sky"] {
        if let Some(sky) = ws.get(*key) {
            let sky = sky.trim();
            if sky.is_empty() { continue; }
            // each face needs the tga OR the bmp, not both
            for face in SKY_FACES {
                let tga = format!("gfx/env/{}{}.tga", sky, face);
                let bmp = format!("gfx/env/{}{}.bmp", sky, face);
                match server_root {
                    Some(root)
                        if find_on_server(&tga, root, search_dirs).is_none()
                            && find_on_server(&bmp, root, search_dirs).is_some() =>
                    {
                        enqueue(&bmp, queue)
                    }
                    _ => enqueue(&tga, queue),
                }
            }
            break;
        }
    }
}

fn collect_entity_resources(entities: &[HashMap<String, String>], queue: &mut HashMap<String, String>) {
    for entity in entities {
        for val in entity.values() {
            if val.is_empty() { continue; }
            let b = val.as_bytes();
            if b.first() == Some(&b'*') && b.get(1).map(|c| c.is_ascii_digit()).unwrap_or(false) {
                continue;
            }
            let lval = val.to_lowercase();
            if lval.ends_with(".wav") || lval.ends_with(".mdl") || lval.ends_with(".spr") {
                enqueue(&normalize_path(val), queue);
            }
        }
    }
}

fn collect_raw_scan(data: &[u8], queue: &mut HashMap<String, String>) {
    fn is_path_char(b: u8) -> bool {
        b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'/' | b'\\')
    }
    let mut i = 0;
    while i < data.len() {
        if !is_path_char(data[i]) { i += 1; continue; }
        let start = i;
        while i < data.len() && is_path_char(data[i]) { i += 1; }
        if i - start < 2 { continue; }
        if data.get(i) != Some(&b'.') { continue; }
        let ext_bytes = match data.get(i + 1..i + 4) {
            Some(e) if e.len() == 3 => e,
            _ => continue,
        };
        let ext = [
            ext_bytes[0].to_ascii_lowercase(),
            ext_bytes[1].to_ascii_lowercase(),
            ext_bytes[2].to_ascii_lowercase(),
        ];
        if ext == *b"wav" || ext == *b"mdl" || ext == *b"spr" {
            if data.get(i + 4).map(|&b| is_path_char(b)).unwrap_or(false) {
                i += 4;
                continue;
            }
            let original = String::from_utf8_lossy(&data[start..i + 4]).into_owned();
            enqueue(&normalize_path(&original), queue);
            i += 4;
        }
    }
}

fn collect_detail_textures(detail_path: &Path, queue: &mut HashMap<String, String>) {
    let file = match File::open(detail_path) { Ok(f) => f, Err(_) => return };
    let reader = io::BufReader::new(file);
    let mut seen: HashSet<String> = HashSet::new();
    for line in reader.lines() {
        let Ok(line) = line else { continue };
        let line = line.trim().to_string();
        if line.is_empty() || line.starts_with("//") { continue; }
        let mut parts = line.split_whitespace();
        let _surface = parts.next();
        let dpath = match parts.next() { Some(p) => p, None => continue };
        if !dpath.to_lowercase().starts_with("detail/") { continue; }
        if !seen.insert(dpath.to_lowercase()) { continue; }
        enqueue(&format!("gfx/{}.tga", dpath), queue);
    }
}

// ============================================================
// FILE LOOKUP
// ============================================================

fn find_on_server(rel: &str, server_root: &Path, search_dirs: &[String]) -> Option<PathBuf> {
    let is_wad = rel.to_lowercase().ends_with(".wad");
    for gd in search_dirs {
        let full = server_root.join(gd).join(rel);
        if full.is_file() { return Some(full); }
        if is_wad {
            if let Some(base) = Path::new(rel).file_name() {
                let full = server_root.join(gd).join(base);
                if full.is_file() { return Some(full); }
            }
        }
    }
    None
}

// ============================================================
// OUTPUT — SERVER MODE
// ============================================================

fn write_zip(
    bsp_path: &Path,
    mapname: &str,
    queue: &HashMap<String, String>,
    vanilla: &HashSet<&str>,
    server_root: &Path,
    search_dirs: &[String],
    out_dir: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let zip_path = out_dir.join(format!("{}.zip", mapname));
    println!("Writing {}...\n", zip_path.display());

    let result = write_zip_inner(bsp_path, mapname, queue, vanilla, server_root, search_dirs, &zip_path);
    if result.is_err() {
        let _ = std::fs::remove_file(&zip_path);
    }
    result
}

fn write_zip_inner(
    bsp_path: &Path,
    mapname: &str,
    queue: &HashMap<String, String>,
    vanilla: &HashSet<&str>,
    server_root: &Path,
    search_dirs: &[String],
    zip_path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let file = File::create(zip_path)?;
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    let mut n_added = 0usize;
    let mut n_vanilla = 0usize;
    let mut n_missing = 0usize;
    let mut n_failed = 0usize;

    let bsp_arc = format!("maps/{}.bsp", mapname);
    zip_add_file(&mut zip, bsp_path, &bsp_arc, options)?;
    println!("  + {:<55} [map]", bsp_arc);
    n_added += 1;

    // optional companions: absent = silent skip
    let optional: HashSet<String> = [
        format!("maps/{}.txt", mapname),
        format!("maps/{}_detail.txt", mapname),
        format!("maps/{}.nav", mapname),
        format!("maps/{}.res", mapname),
        format!("overviews/{}.bmp", mapname),
        format!("overviews/{}.txt", mapname),
    ].into_iter().map(|s| s.to_lowercase()).collect();

    let res_lc = format!("maps/{}.res", mapname).to_lowercase();
    let mut have_res = false;
    let mut added_rels: Vec<String> = Vec::new();

    let mut sorted_keys: Vec<&String> = queue.keys().collect();
    sorted_keys.sort();

    for lc_rel in sorted_keys {
        let rel = &queue[lc_rel];

        if vanilla.contains(lc_rel.as_str()) {
            println!("  - {:<55} [vanilla]", rel);
            n_vanilla += 1;
            continue;
        }

        match find_on_server(rel, server_root, search_dirs) {
            None if optional.contains(lc_rel) => {}
            None => {
                println!("  ? {:<55} [not found]", rel);
                n_missing += 1;
            }
            // read fully before starting the entry so a failure can't corrupt the zip
            Some(full) => match std::fs::read(&full) {
                Err(e) => {
                    println!("  ! {:<55} [read error: {}]", rel, e);
                    n_failed += 1;
                }
                Ok(data) => {
                    zip.start_file(rel.as_str(), options)?;
                    zip.write_all(&data)?;
                    println!("  + {:<55} [added]", rel);
                    n_added += 1;
                    added_rels.push(rel.clone());
                    if *lc_rel == res_lc { have_res = true; }
                }
            },
        }
    }

    // no .res on the server: generate one (no .nav — clients don't need it)
    if !have_res {
        let resource_lines: String = added_rels.iter()
            .filter(|r| !r.to_lowercase().ends_with(".nav"))
            .map(|r| format!("{}\n", r))
            .collect();
        let timestamp = OffsetDateTime::now_utc().format(&Rfc3339)?;
        let res_body = format!(
            "// resources for {}.bsp\n// generated by packmap {}\n{}",
            mapname, timestamp, resource_lines
        );
        let res_arc = format!("maps/{}.res", mapname);
        zip.start_file(res_arc.as_str(), options)?;
        zip.write_all(res_body.as_bytes())?;
        println!("  + {:<55} [generated]", res_arc);
        n_added += 1;
    }

    zip.finish()?;

    println!("\n--- Summary ---");
    println!("  Added:   {} files", n_added);
    println!("  Vanilla: {} skipped", n_vanilla);
    println!("  Missing: {} not found on server", n_missing);
    if n_failed > 0 {
        println!("  Failed:  {} unreadable, left out of the zip", n_failed);
    }
    println!("  Output:  {}", zip_path.display());

    Ok(())
}

fn zip_add_file(
    zip: &mut ZipWriter<File>,
    disk_path: &Path,
    arc_name: &str,
    options: SimpleFileOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    let data = std::fs::read(disk_path)?;
    zip.start_file(arc_name, options)?;
    zip.write_all(&data)?;
    Ok(())
}

// ============================================================
// OUTPUT — LOOSE MODE
// ============================================================

fn write_resource_list(
    mapname: &str,
    queue: &HashMap<String, String>,
    vanilla: &HashSet<&str>,
    out_dir: &Path,
    server_root: Option<&Path>,
    search_dirs: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    let res_path = out_dir.join(format!("{}.res", mapname));

    // companions we probe for but can't verify exist without server access
    let excluded: HashSet<String> = [
        format!("maps/{}.txt", mapname).to_lowercase(),
        format!("maps/{}_detail.txt", mapname).to_lowercase(),
        format!("maps/{}.nav", mapname).to_lowercase(),
        format!("maps/{}.res", mapname).to_lowercase(),
        format!("overviews/{}.bmp", mapname).to_lowercase(),
        format!("overviews/{}.txt", mapname).to_lowercase(),
    ].into_iter().collect();

    let mut needed: Vec<String> = Vec::new();
    let mut sorted_keys: Vec<&String> = queue.keys().collect();
    sorted_keys.sort();

    for lc_rel in sorted_keys {
        if vanilla.contains(lc_rel.as_str()) { continue; }
        if server_root.is_none() && excluded.contains(lc_rel.as_str()) { continue; }
        if lc_rel == &format!("maps/{}.nav", mapname).to_lowercase() { continue; }
        if lc_rel == &format!("maps/{}.res", mapname).to_lowercase() { continue; }
        let rel = queue[lc_rel].clone();
        if let Some(root) = server_root {
            if find_on_server(&rel, root, search_dirs).is_none() { continue; }
        }
        println!("  {}", rel);
        needed.push(rel);
    }

    let mut res = File::create(&res_path)?;
    writeln!(res, "// resources for {}.bsp", mapname)?;
    writeln!(
        res,
        "// generated by packmap {}",
        OffsetDateTime::now_utc().format(&Rfc3339)?
    )?;
    for r in &needed { writeln!(res, "{}", r)?; }

    println!("\n--- Summary ---");
    println!("  Resources: {} non-vanilla files listed", needed.len());
    println!("  Output:    {}", res_path.display());

    Ok(())
}
