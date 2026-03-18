# media-index

Indexador y deduplicador de archivos multimedia escrito en Rust. Escanea directorios recursivamente, extrae metadatos, detecta duplicados por hash de contenido, genera thumbnails y guarda todo en una base de datos SQLite local.

---

## Características

- **Deduplicación por contenido** usando BLAKE3 — detecta duplicados aunque tengan nombres distintos
- **Metadatos ricos** para audio, video, imagen y modelos 3D
- **Soporte de comprimidos** — indexa el contenido de `.zip`, `.7z` y `.rar` sin extraerlos
- **Cero archivos perdidos** — cualquier extensión desconocida o archivo sin extensión se indexa igual
- **Limpieza automática** — al re-escanear elimina de la BD los archivos que ya no existen en disco
- **Canónico inteligente** — si hay `superman.stl` y `superman - copia.stl`, siempre elige el original como canónico
- **Thumbnails** — generación de previsualizaciones para imágenes (con corrección EXIF), videos y modelos 3D
- **Watch mode** — vigila un directorio en tiempo real e indexa cambios al instante
- **Borrado inteligente de duplicados** con tres modos de operación
- **Paralelismo** con rayon para escaneos rápidos en colecciones grandes

---

## Instalación

### Requisitos

- Rust 1.85+ (`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`)

### Dependencias opcionales

| Herramienta | Para qué | Instalar |
|-------------|----------|---------|
| `ffprobe` | Metadatos de video (duración, resolución, códec) | `sudo apt install ffmpeg` / `brew install ffmpeg` |
| `ffmpeg` | Thumbnails de video | Incluido con ffprobe |
| `unrar` | Leer archivos `.rar` | `sudo apt install unrar` / `brew install rar` |
| `stl-thumb` | Thumbnails 3D con OpenGL (mejor calidad) | [Releases en GitHub](https://github.com/unlimitedbacon/stl-thumb/releases) |

Sin estas herramientas el programa sigue funcionando: los videos se indexan sin metadatos, los `.rar` se omiten, y los thumbnails 3D se generan con el renderer interno (sin GPU).

### Compilar

```bash
git clone https://github.com/usuario/media-index
cd media-index
cargo build --release
# El binario queda en target/release/media-index
```

---

## Uso rápido

```bash
# Escanear un directorio
media-index scan ./mis-fotos

# Ver estadísticas
media-index stats

# Ver duplicados
media-index dupes

# Generar thumbnails
media-index thumbs

# Buscar archivos
media-index search "vacation"

# Vigilar cambios en tiempo real
media-index watch ./mis-fotos
```

---

## Referencia de comandos

La base de datos por defecto es `media.db` en el directorio actual. Se puede cambiar con `-d`:

```bash
media-index -d /ruta/coleccion.db <comando>
```

---

### `scan` — Escanear un directorio

Indexa recursivamente todos los archivos de un directorio, incluyendo el contenido de comprimidos. Al inicio de cada escaneo limpia automáticamente las entradas de la BD cuyos archivos ya no existen en disco.

```bash
media-index scan <PATH> [--verbose]
```

| Argumento | Descripción |
|-----------|-------------|
| `PATH` | Directorio a escanear (obligatorio) |
| `-v, --verbose` | Mostrar cada archivo procesado y los duplicados encontrados |

**Ejemplo:**

```bash
media-index scan ~/Pictures --verbose
```

**Salida:**

```
Escaneando: /home/usuario/Pictures
→ 1,842 archivos encontrados

⠸ [00:00:12] [████████████████████░░░░] 1501/1842 foto_vacaciones.jpg

─── Resultado ────────────────────────────────
  0 archivos 3D
  12 videos
  234 audios
  1580 imágenes
  16 otros
  4 comprimidos
  23 duplicados (1.2 GB)
  ──────────────────────────────────────
  1842 indexados en total
```

---

### `watch` — Vigilar cambios en tiempo real

Realiza un escaneo inicial completo y luego se queda vigilando el directorio. Cuando se crea, modifica o borra un archivo, lo procesa automáticamente sin re-escanear todo.

```bash
media-index watch <PATH> [--verbose] [--debounce <SEGUNDOS>]
```

| Argumento | Descripción |
|-----------|-------------|
| `PATH` | Directorio a vigilar (obligatorio) |
| `-v, --verbose` | Mostrar detalles de cada archivo procesado |
| `-d, --debounce` | Segundos de espera antes de procesar un evento (default: `2`) |

El debounce agrupa eventos del OS que ocurren en ráfaga (como copiar una carpeta entera) para procesarlos de una sola vez.

```bash
media-index watch ~/Downloads --debounce 5
```

Presionar `Ctrl+C` para detener.

---

### `stats` — Estadísticas de la colección

```bash
media-index stats
```

**Salida:**

```
─── Colección ────────────────────────────────
  Total único    : 1,842
  Duplicados     : 23
  Tamaño total   : 48.3 GB
  Lib. por dedup : 1.2 GB

  Archivos       Tamaño  Tipo
  ────────────────────────────────────
     1,580     32.1 GB  🖼 IMAGE
       234      8.4 GB  ♪ AUDIO
        12      7.2 GB  ▶ VIDEO
        16    512.0 MB  · OTHER
         0        0 B   ⬡ 3D
```

---

### `dupes` — Listar y borrar duplicados

```bash
media-index dupes [--tipo <TIPO>] [--json] [--delete] [--aggressive] [--prefer-archive]
```

| Argumento | Descripción |
|-----------|-------------|
| `-t, --tipo` | Filtrar: `td`, `video`, `audio`, `imagen`, `otro` |
| `-j, --json` | Salida en formato JSON |
| `-d, --delete` | Borrar duplicados sueltos en disco |
| `-a, --aggressive` | Con `--delete`: borrar el comprimido entero si **todo** su contenido son duplicados |
| `-p, --prefer-archive` | Con `--delete`: borrar el archivo suelto si ya existe dentro de un comprimido |

#### Solo listar

```bash
media-index dupes
media-index dupes --tipo video
media-index dupes --json
```

**Salida:**

```
3 grupos duplicados  —  1.2 GB liberables

● ♪ AUD cancion_favorita.mp3 (8.4 MB)
  a1b2c3d4e5f6a7b8
  /musica/originales/cancion_favorita.mp3
  ↳ /musica/backup/cancion_favorita.mp3
  ⊡ /backups/musica.zip::cancion_favorita.mp3
```

`↳` = duplicado suelto en disco. `⊡` = duplicado dentro de un comprimido.

#### Borrar duplicados (`--delete`)

Borra únicamente los duplicados sueltos en disco. Los que están dentro de comprimidos se reportan pero no se tocan.

```bash
media-index dupes --delete
```

#### Borrar comprimidos completos (`--aggressive`)

Si **todos** los archivos de un comprimido son duplicados de archivos que ya existen en otro lugar, borra el comprimido entero. Si tiene aunque sea un archivo único, no se toca.

```bash
media-index dupes --delete --aggressive
```

#### Preferir archivos en comprimidos (`--prefer-archive`)

Si el canónico es un archivo suelto y **todos** sus duplicados conocidos están dentro de comprimidos, borra el archivo suelto. El contenido sigue intacto dentro del comprimido.

```bash
media-index dupes --delete --prefer-archive
```

#### Combinaciones

```bash
# Limpieza completa
media-index dupes --delete --aggressive --prefer-archive

# Solo duplicados de video en JSON
media-index dupes --tipo video --json > duplicados_video.json
```

> Después de borrar con cualquiera de estos modos, el próximo `scan` actualiza la BD automáticamente.

---

### `thumbs` — Generar thumbnails

Genera previsualizaciones JPEG para imágenes, videos y modelos 3D. Los thumbnails se guardan en `media.thumbs/` junto a la BD, organizados en subdirectorios por los primeros 2 caracteres del hash.

```bash
media-index thumbs [--tipo <TIPO>] [--size <PX>] [--quality <1-100>] [--force] [--verbose]
```

| Argumento | Descripción |
|-----------|-------------|
| `-t, --tipo` | Filtrar: `td`, `video`, `audio`, `imagen`, `otro` |
| `-s, --size` | Tamaño en píxeles del lado (default: `256`) |
| `-q, --quality` | Calidad JPEG 1–100 (default: `85`) |
| `-f, --force` | Regenerar thumbnails que ya existen |
| `-v, --verbose` | Mostrar errores detallados por cada archivo |

**Ejemplos:**

```bash
# Generar todo
media-index thumbs

# Solo imágenes, tamaño mayor
media-index thumbs --tipo imagen --size 512

# Regenerar con detalle de errores
media-index thumbs --force --verbose

# Regenerar solo los 3D
media-index thumbs --force --tipo td
```

**Cómo genera cada tipo:**

- **Imágenes** — aplica corrección de orientación EXIF automáticamente (fotos de cámara/móvil giradas), luego redimensiona manteniendo proporción
- **Video** — extrae un frame en el segundo 5 (o el 0 si el video es más corto) usando `ffmpeg`
- **3D (STL/OBJ/3MF)** — si `stl-thumb` está instalado lo usa (OpenGL, FXAA, alta calidad); si no, usa el renderer interno (proyección isométrica con flat shading y z-buffer, sin GPU)

Los thumbnails se generan también para archivos indexados **dentro de comprimidos** — se extraen temporalmente en memoria, se genera el thumbnail y el archivo temporal se borra.

Por defecto salta los thumbnails que ya existen. Con `--force` los regenera todos.

#### Thumbnails 3D con stl-thumb

`stl-thumb` usa OpenGL para renderizar y produce thumbnails de mucho mayor calidad que el renderer interno. Si está instalado se detecta y usa automáticamente para STL, OBJ y 3MF.

Si falla (por ejemplo en WSL2 sin display), el programa lo indica y usa el renderer interno:

```
⚠ stl-thumb falló, usando renderer interno: stl-thumb exit 1: ...
```

Para habilitarlo en WSL2:

```bash
# Mesa software rendering (sin GPU)
export LIBGL_ALWAYS_SOFTWARE=1
media-index thumbs --tipo td

# O verificar que WSLg está activo (Windows 11)
glxinfo | grep "OpenGL renderer"
```

---

### `search` — Buscar archivos

Busca archivos por nombre (búsqueda parcial, sin distinguir mayúsculas). Devuelve hasta 200 resultados.

```bash
media-index search <QUERY> [--tipo <TIPO>]
```

```bash
media-index search "vacation" --tipo imagen
```

**Salida:**

```
3 resultados

▸ [IMG] vacation_beach.jpg
  /home/usuario/Pictures/2023/vacation_beach.jpg
  4.2 MB · 4032×3024px · iPhone 14 Pro
```

---

### `export` — Exportar a JSON

```bash
media-index export [--output <ARCHIVO>]
```

| Argumento | Descripción |
|-----------|-------------|
| `-o, --output` | Ruta del archivo de salida (default: `media_export.json`) |

El JSON incluye estadísticas generales y la lista completa de grupos duplicados:

```json
{
  "stats": {
    "total": 1842,
    "duplicates": 23,
    "bytes": 51843072000,
    "by_type": [["image", 1580, 34494832640]]
  },
  "duplicates": [
    {
      "hash": "a1b2c3d4...",
      "media_type": "audio",
      "canonical_name": "cancion.mp3",
      "canonical_path": "/musica/originales/cancion.mp3",
      "size_bytes": 8808038,
      "duplicates": ["/musica/backup/cancion.mp3"]
    }
  ]
}
```

---

### `clear` — Borrar la base de datos

Elimina el archivo `.db`, `.db-wal` y `.db-shm`. Pide confirmación interactiva salvo con `--force`.

```bash
media-index clear [--force]
```

```bash
# Interactivo
media-index clear

# Sin confirmación
media-index clear --force

# Limpiar y re-escanear desde cero
media-index clear --force && media-index scan ./fotos
```

---

### `doctor` — Verificar dependencias

```bash
media-index doctor
```

**Salida:**

```
─── Diagnóstico de dependencias ──────────────
  ✓ ffprobe (metadatos de video)
  ✓ unrar (archivos .rar)
  ✓ stl-thumb (thumbnails 3D con OpenGL — mejor calidad)
    · stl-thumb -V → exit=0 stdout="stl-thumb 0.5.0"

  ✓ ZIP, 7Z, audio, imagen: pure Rust — sin dependencias
```

---

## Formatos soportados

### Con metadatos completos

| Tipo | Extensiones |
|------|-------------|
| **Imagen** | `jpg` `jpeg` `png` `webp` `tiff` `tif` `bmp` `gif` `avif` `heic` `heif` `raw` `cr2` `cr3` `nef` `arw` `dng` `orf` `rw2` `psd` `xcf` |
| **Audio** | `mp3` `flac` `ogg` `opus` `m4a` `aac` `wav` `aiff` `aif` `wma` `ape` `wv` `mka` `alac` `dsf` `dff` |
| **Video** | `mp4` `mkv` `avi` `mov` `wmv` `flv` `webm` `m4v` `mpg` `mpeg` `ts` `mts` `m2ts` `vob` `divx` `xvid` `rmvb` `3gp` |
| **3D** | `stl` `obj` `3mf` |

Los metadatos de **video** requieren `ffprobe`. Los thumbnails de **video** requieren `ffmpeg`.

### Sin metadatos (solo hash + ruta)

Cualquier extensión no listada, archivos sin extensión (`Makefile`, `Dockerfile`, `LICENSE`, etc.) y archivos dentro de comprimidos con extensión desconocida. Se indexan, se deduplicán y aparecen en búsquedas con el tipo `other`.

### Comprimidos

| Formato | Soporte | Notas |
|---------|---------|-------|
| `.zip` | Nativo (pure Rust) | Sin dependencias externas |
| `.7z` | Nativo (pure Rust) | Sin dependencias externas |
| `.rar` | Requiere `unrar` | Multi-part soportado |

Los archivos multi-part (`.part1.rar`, `.7z.001`) se procesan como una sola unidad.

---

## Base de datos

El índice se guarda en SQLite con WAL mode. El esquema principal:

```
files           — un registro por contenido único (hash BLAKE3)
duplicates      — paths adicionales con el mismo contenido
meta_image      — metadatos EXIF de imágenes
meta_audio      — tags ID3/Vorbis/etc de audio
meta_video      — metadatos de video vía ffprobe
meta_3d         — geometría de modelos 3D (triángulos, vértices, dimensiones)
```

### Deduplicación

Cada archivo se identifica por su hash BLAKE3:

- **Archivos ≤ 100 MB**: hash del contenido completo
- **Archivos > 100 MB**: hash parcial (primeros 4 MB + últimos 4 MB + tamaño)

El primer path donde se encuentra un contenido se convierte en el **canónico**. Si después aparece otro path con el mismo hash pero nombre más original (sin patrones de copia como `- copia`, `- Copy`, `(1)`, `_backup`, etc.), ese path es promovido a canónico automáticamente.

Al re-escanear, si el canónico ya no existe en disco, `cleanup_stale` lo elimina y el próximo archivo con ese hash se convierte en el nuevo canónico.

### Thumbnails

Los thumbnails se guardan en `{nombre_bd}.thumbs/` junto al archivo de BD:

```
media.db
media.thumbs/
  a1/
    a1b2c3d4...hash....jpg
  f3/
    f3e8a9b1...hash....jpg
```

El nombre del thumbnail es el hash BLAKE3 completo del archivo original, lo que garantiza que no hay colisiones y que un mismo contenido siempre produce el mismo thumbnail.

---

## Ejemplos de uso avanzado

```bash
# Usar una BD con nombre personalizado
media-index -d peliculas.db scan ~/Videos

# Escanear múltiples directorios en la misma BD
media-index scan ~/Photos
media-index scan /mnt/backup/Photos

# Ver duplicados solo de audio en JSON y procesarlos con jq
media-index dupes --tipo audio --json | jq '.[] | .canonical_path'

# Limpieza completa y agresiva
media-index dupes --delete --aggressive --prefer-archive

# Thumbnails en alta resolución
media-index thumbs --size 512 --quality 95

# Watch con BD personalizada y debounce largo (útil en redes lentas)
media-index -d /nas/index.db watch /nas/media --debounce 10

# Re-indexar desde cero
media-index clear --force && media-index scan ./fotos

# Exportar y hacer backup del índice
media-index export --output "index_$(date +%Y%m%d).json"

# stl-thumb en WSL2 sin GPU
LIBGL_ALWAYS_SOFTWARE=1 media-index thumbs --tipo td --force
```

---

## Estructura del proyecto

```
src/
├── main.rs          — CLI (clap), comandos y lógica de presentación
├── scanner.rs       — Escaneo paralelo (rayon), watch mode, indexado individual
├── db.rs            — Capa de acceso a SQLite, inserción, consultas, limpieza
├── models.rs        — Tipos de datos: MediaType, MediaEntry, ScanStats, etc.
├── archive.rs       — Extracción de comprimidos (zip, 7z, rar)
├── thumbs.rs        — Generación de thumbnails (imagen, video, 3D)
└── parsers/
    ├── mod.rs       — Dispatcher: elige el parser según el tipo
    ├── audio.rs     — Tags con lofty
    ├── image.rs     — Dimensiones + EXIF con image + kamadak-exif
    ├── video.rs     — Metadatos vía ffprobe (subproceso)
    └── print3d.rs   — Geometría de STL/OBJ/3MF
```
