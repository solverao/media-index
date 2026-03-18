# media-index

Indexador y deduplicador de archivos 3D, video, audio e imagen.  
Escala a colecciones de +1 TB con deduplicación por contenido (BLAKE3).

---

## Tipos soportados

| Tipo   | Extensiones |
|--------|-------------|
| **3D** | STL, OBJ, 3MF |
| **Video** | MP4, MKV, AVI, MOV, WMV, FLV, WebM, M4V, MPG, TS, MTS, M2TS, VOB, DIVX, 3GP… |
| **Audio** | MP3, FLAC, OGG, Opus, M4A, AAC, WAV, AIFF, WMA, APE, WavPack, ALAC, DSF… |
| **Imagen** | JPG, PNG, WebP, TIFF, BMP, GIF, AVIF, HEIC, RAW, CR2, CR3, NEF, ARW, DNG, PSD… |
| **Comprimidos** | ZIP, RAR, 7Z (todos con soporte multi-part) |

---

## Instalación

```bash
# Rust (https://rustup.rs)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
cargo build --release
```

### Dependencias externas (opcionales)

```bash
# ffprobe — metadatos de video (duración, resolución, codec, fps)
sudo apt install ffmpeg        # Debian/Ubuntu
brew install ffmpeg            # macOS

# unrar — abrir archivos .rar
sudo apt install unrar
brew install rar
```

ZIP y 7Z funcionan sin instalar nada extra. Audio e imagen son pure Rust.

---

## Uso

### Verificar dependencias
```bash
./media-index doctor
```

### Escanear colección
```bash
./media-index scan /mnt/disco/media
./media-index scan /mnt/disco --verbose
./media-index --db mi_coleccion.db scan /mnt/disco
```

### Estadísticas
```bash
./media-index stats
```
```
─── Colección ────────────────────────────────
  Total único    :  47,293
  Duplicados     :   8,102
  Tamaño total   :  1.24 TB
  Lib. por dedup :  189 GB

    Archivos        Tamaño  Tipo
  ────────────────────────────────────
      12847     847.3 GB  ▶ VIDEO
      18431      84.2 GB  ♪ AUDIO
       9102     290.1 GB  🖼 IMG
       6913      18.4 GB  ⬡ 3D
```

### Duplicados
```bash
# Ver todos
./media-index dupes

# Solo videos
./media-index dupes --tipo video

# Exportar para procesar en Laravel
./media-index dupes --json > duplicados.json
```

### Buscar
```bash
./media-index search "breaking bad"
./media-index search "benchy" --tipo td
./media-index search "beethoven" --tipo audio
```

### Exportar índice completo
```bash
./media-index export --output indice.json
```

---

## Metadatos extraídos

### 3D (STL / OBJ / 3MF)
`triangle_count`, `vertex_count`, `object_count`, `dim_x/y/z`

### Video (vía ffprobe)
`duration`, `width × height`, `codec_video`, `codec_audio`, `fps`, `bitrate`, `title`, `year`, `container`

### Audio (pure Rust — lofty)
`duration`, `bitrate`, `sample_rate`, `channels`, `title`, `artist`, `album`, `year`, `genre`, `track`

### Imagen (pure Rust — image + kamadak-exif)
`width × height`, `camera_make`, `camera_model`, `taken_at`, `gps_lat/lon`, `iso`, `focal_length`

---

## Hashing inteligente

| Tamaño del archivo | Estrategia |
|---|---|
| < 100 MB | BLAKE3 completo |
| ≥ 100 MB | BLAKE3 de cabeza (4 MB) + cola (4 MB) + tamaño |

Los archivos de video grandes no se cargan completos en RAM.
La estrategia parcial es suficiente para deduplicación práctica.

---

## Schema SQLite

```sql
files        -- tabla principal (un registro por hash único)
meta_3d      -- metadatos de archivos 3D
meta_video   -- metadatos de video
meta_audio   -- metadatos de audio
meta_image   -- metadatos de imagen
duplicates   -- grupos de duplicados
```

### Consultas útiles

```sql
-- Top 20 videos más pesados
SELECT f.original_name, f.size_bytes, v.duration_secs,
       v.width, v.height, v.codec_video
FROM files f JOIN meta_video v ON v.file_id = f.id
ORDER BY f.size_bytes DESC LIMIT 20;

-- Discografía de un artista
SELECT a.artist, a.album, a.track_number, a.title, f.original_name
FROM meta_audio a JOIN files f ON f.id = a.file_id
WHERE a.artist LIKE '%Pink Floyd%'
ORDER BY a.album, a.track_number;

-- Fotos con GPS
SELECT f.original_name, i.gps_lat, i.gps_lon, i.taken_at, i.camera_model
FROM meta_image i JOIN files f ON f.id = i.file_id
WHERE i.gps_lat IS NOT NULL;

-- Espacio desperdiciado por duplicados
SELECT f.media_type, COUNT(*) as grupos,
       SUM(f.size_bytes * (SELECT COUNT(*) FROM duplicates d WHERE d.canonical_id = f.id)) as bytes_dup
FROM files f
WHERE EXISTS (SELECT 1 FROM duplicates d WHERE d.canonical_id = f.id)
GROUP BY f.media_type;
```

---

## Integración con Laravel

```php
// config/database.php
'media_index' => [
    'driver'   => 'sqlite',
    'database' => storage_path('media.db'),
],

// Uso en controller o Livewire
$videos = DB::connection('media_index')
    ->table('files as f')
    ->join('meta_video as v', 'v.file_id', '=', 'f.id')
    ->where('f.media_type', 'video')
    ->where('v.title', 'like', "%{$query}%")
    ->select('f.*', 'v.duration_secs', 'v.width', 'v.height', 'v.codec_video')
    ->get();
```

## Eliminar 

### Solo listar (como siempre)
media-index dupes

### Borrar duplicados sueltos, reportar los que están en comprimidos
media-index dupes --delete

### Borrar sueltos + borrar comprimidos donde TODO es duplicado
media-index dupes --delete --aggressive

### Filtrar por tipo
media-index dupes --delete --tipo video
```

#### Comportamiento

**Sin `--aggressive`** — solo toca archivos en disco:
```
✓ /fotos/backup/imagen.jpg
⊡ /backups/archivo.zip::imagen.jpg   ← reportado, no tocado
⊡ /backups/otro.zip::video.mp4       ← reportado, no tocado
→ Usa --aggressive para borrar el comprimido si todos sus archivos son duplicados.
```

**Con `--aggressive`** — evalúa cada comprimido:
```
✓ comprimido completo: /backups/archivo.zip   ← todos sus archivos eran duplicados
⊡ /backups/otro.zip — tiene archivos únicos, no se borra (1 duplicado(s) dentro)

### Borrar si el canónico es un archivo suelto en disco y todos sus duplicados están dentro de comprimidos:

```
media-index dupes --delete --prefer-archive
```

### Combinado con --aggressive (borra sueltos + comprimidos completos duplicados):

```
media-index dupes --delete --prefer-archive --aggressive
```