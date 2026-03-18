# media-index

Indexador y deduplicador de archivos multimedia escrito en Rust. Escanea directorios recursivamente, extrae metadatos, detecta duplicados por hash de contenido y guarda todo en una base de datos SQLite local.

---

## Características

- **Deduplicación por contenido** usando BLAKE3 — detecta duplicados aunque tengan nombres distintos
- **Metadatos ricos** para audio, video, imagen y modelos 3D
- **Soporte de comprimidos** — indexa el contenido de `.zip`, `.7z` y `.rar` sin extraerlos
- **Cero archivos perdidos** — cualquier extensión desconocida se indexa igual (hash + ruta)
- **Limpieza automática** — al re-escanear elimina de la BD los archivos que ya no existen en disco
- **Watch mode** — vigila un directorio en tiempo real e indexa cambios al instante
- **Borrado inteligente de duplicados** con tres modos de operación
- **Paralelismo** con rayon para escaneos rápidos en colecciones grandes
- **Thumbnails** Generar thumbnails de imágenes + video + 3D
---

## Instalación

### Requisitos

- Rust 1.85+ (`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`)
- **Opcional** — `ffprobe` para metadatos de video (duración, resolución, códec)
- **Opcional** — `unrar` para leer archivos `.rar`

```bash
# ffprobe
sudo apt install ffmpeg        # Debian/Ubuntu
brew install ffmpeg            # macOS

# unrar
sudo apt install unrar         # Debian/Ubuntu
brew install rar               # macOS
```

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

# Buscar archivos
media-index search "vacation"

# Vigilar cambios en tiempo real
media-index watch ./mis-fotos

# Generar thumbnails de todo (imágenes + video + 3D)
media-index thumbs

# Solo imágenes, tamaño personalizado
media-index thumbs --tipo imagen --size 512

# Regenerar todo desde cero
media-index thumbs --force

# Calidad baja para ahorrar espacio
media-index thumbs --quality 60

# BD personalizada
media-index -d coleccion.db thumbs
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

El debounce agrupa eventos del OS que ocurren en ráfaga (como copiar una carpeta entera) para procesarlos de una sola vez en lugar de uno por uno.

**Ejemplo:**

```bash
# Vigilar con debounce de 5 segundos
media-index watch ~/Downloads --debounce 5

# Salida
Escaneo inicial: /home/usuario/Downloads
  842 indexados  3 duplicados

👁 Vigilando /home/usuario/Downloads  (debounce 5s — Ctrl+C para salir)
  → pelicula.mkv
  → cancion.mp3
    duplicado /home/usuario/Downloads/cancion.mp3
```

Presionar `Ctrl+C` para detener.

---

### `stats` — Estadísticas de la colección

Muestra un resumen del estado actual de la BD.

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

Lista todos los grupos de duplicados. Opcionalmente los borra.

```bash
media-index dupes [--tipo <TIPO>] [--json] [--delete] [--aggressive] [--prefer-archive]
```

| Argumento | Descripción |
|-----------|-------------|
| `-t, --tipo` | Filtrar por tipo: `td`, `video`, `audio`, `imagen`, `otro` |
| `-j, --json` | Salida en formato JSON |
| `-d, --delete` | Borrar duplicados sueltos en disco |
| `-a, --aggressive` | Con `--delete`: borrar el comprimido entero si **todo** su contenido son duplicados |
| `-p, --prefer-archive` | Con `--delete`: si el canónico es un archivo suelto y ya existe dentro de un comprimido, borrar el archivo suelto |

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

El marcador `↳` indica un duplicado suelto en disco. El marcador `⊡` indica un duplicado dentro de un comprimido.

#### Borrar duplicados (`--delete`)

Borra únicamente los duplicados sueltos en disco. Los que están dentro de comprimidos se reportan pero no se tocan.

```bash
media-index dupes --delete
```

```
  ✓ /musica/backup/cancion_favorita.mp3
  ⊡ /backups/musica.zip::cancion_favorita.mp3  ← reportado, no borrado
  → Usa --aggressive para borrar el comprimido si todos sus archivos son duplicados.

─── Resultado ────────────────────────────────
  1 archivo(s) borrado(s)
  8.4 MB liberados
```

#### Borrar comprimidos completos (`--aggressive`)

Si **todos** los archivos de un comprimido son duplicados de archivos que ya existen en otro lugar, borra el comprimido entero. Si el comprimido tiene aunque sea un archivo único, no se toca.

```bash
media-index dupes --delete --aggressive
```

```
  ✓ /musica/backup/cancion_favorita.mp3
  ✓ comprimido completo: /backups/musica_vieja.zip
  ⊡ /backups/coleccion.zip — tiene archivos únicos, no se borra (2 duplicado(s) dentro)
```

#### Preferir archivos en comprimidos (`--prefer-archive`)

Caso de uso: tienes un archivo suelto en disco y una copia del mismo dentro de un `.zip`. El suelto quedó como canónico pero quieres conservar solo el comprimido.

Con esta opción, si el canónico es un archivo suelto y **todos** sus duplicados conocidos están dentro de comprimidos, borra el archivo suelto. El contenido sigue intacto dentro del comprimido.

```bash
media-index dupes --delete --prefer-archive
```

```
  ✓ /fotos/foto.jpg  (canónico suelto — copia en comprimido)
```

> **Nota:** tras borrar archivos con cualquiera de estos modos, el próximo `scan` o `watch` actualizará la BD automáticamente mediante la limpieza de entradas obsoletas.

#### Combinaciones habituales

```bash
# Limpieza completa y agresiva
media-index dupes --delete --aggressive --prefer-archive

# Solo duplicados de video, en JSON para procesar externamente
media-index dupes --tipo video --json > duplicados_video.json
```

---

### `search` — Buscar archivos

Busca archivos por nombre (búsqueda parcial, sin distinguir mayúsculas).

```bash
media-index search <QUERY> [--tipo <TIPO>]
```

| Argumento | Descripción |
|-----------|-------------|
| `QUERY` | Texto a buscar en el nombre del archivo |
| `-t, --tipo` | Filtrar por tipo: `td`, `video`, `audio`, `imagen`, `otro` |

**Ejemplo:**

```bash
media-index search "vacation" --tipo imagen
```

**Salida:**

```
3 resultados

▸ [IMG] vacation_beach.jpg
  /home/usuario/Pictures/2023/vacation_beach.jpg
  4.2 MB · 4032×3024px · iPhone 14 Pro

▸ [IMG] vacation_sunset.heic
  /home/usuario/Pictures/2023/vacation_sunset.heic
  6.1 MB · 4032×3024px · iPhone 14 Pro
```

Devuelve hasta 200 resultados por búsqueda.

---

### `export` — Exportar a JSON

Exporta el índice completo y la lista de duplicados a un archivo JSON.

```bash
media-index export [--output <ARCHIVO>]
```

| Argumento | Descripción |
|-----------|-------------|
| `-o, --output` | Ruta del archivo de salida (default: `media_export.json`) |

**Ejemplo:**

```bash
media-index export --output backup_index.json
```

El JSON tiene la siguiente estructura:

```json
{
  "stats": {
    "total": 1842,
    "duplicates": 23,
    "bytes": 51843072000,
    "by_type": [["image", 1580, 34494832640], ...]
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

Elimina completamente la base de datos (archivo `.db`, `.db-wal` y `.db-shm`). Pide confirmación interactiva salvo que se use `--force`.

```bash
media-index clear [--force]
```

| Argumento | Descripción |
|-----------|-------------|
| `-f, --force` | No pedir confirmación (útil en scripts) |

**Ejemplo:**

```bash
# Interactivo
media-index clear
⚠ Esto borrará media.db por completo y no se puede deshacer.
  ¿Continuar? [s/N] s
✓ Base de datos eliminada: media.db

# Sin confirmación
media-index clear --force

# Limpiar y re-escanear desde cero
media-index clear --force && media-index scan ./fotos
```

---

### `doctor` — Verificar dependencias

Comprueba si las dependencias opcionales están instaladas.

```bash
media-index doctor
```

**Salida:**

```
─── Diagnóstico de dependencias ──────────────
  ✓ ffprobe (metadatos de video)
  ✗ unrar (archivos .rar) — instalar: sudo apt install unrar / brew install rar

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

Los metadatos de **video** requieren `ffprobe`. Sin él se indexan igualmente pero sin duración, resolución ni códec.

### Sin metadatos (solo hash + ruta)

Cualquier extensión no listada arriba, archivos sin extensión (`Makefile`, `Dockerfile`, `LICENSE`, etc.) y archivos dentro de comprimidos con extensión desconocida. Se indexan, se deduplicanb y aparecen en búsquedas con el tipo `other`.

### Comprimidos

| Formato | Soporte | Notas |
|---------|---------|-------|
| `.zip` | Nativo (pure Rust) | Sin dependencias externas |
| `.7z` | Nativo (pure Rust) | Sin dependencias externas |
| `.rar` | Requiere `unrar` | Multi-part soportado |

Los archivos multi-part (`.part1.rar`, `.7z.001`) se procesan como una sola unidad.

---

## Base de datos

El índice se guarda en un archivo SQLite. El esquema principal:

```
files           — un registro por contenido único (identificado por hash BLAKE3)
duplicates      — paths adicionales con el mismo contenido que un archivo en files
meta_image      — metadatos EXIF de imágenes
meta_audio      — tags ID3/Vorbis/etc de audio
meta_video      — metadatos de video vía ffprobe
meta_3d         — geometría de modelos 3D
```

La BD usa WAL mode para mejor rendimiento en lecturas concurrentes. Las tablas de metadatos tienen `ON DELETE CASCADE`, por lo que borrar un registro de `files` limpia automáticamente todos sus metadatos y duplicados asociados.

### Deduplicación

Cada archivo se identifica por su hash BLAKE3:

- **Archivos ≤ 100 MB**: hash del contenido completo
- **Archivos > 100 MB**: hash parcial (primeros 4 MB + últimos 4 MB + tamaño del archivo) para evitar leer archivos de video enormes completos en RAM

El primer path donde se encuentra un contenido se convierte en el **canónico** (guardado en `files`). Los paths adicionales con el mismo hash se guardan en `duplicates`.

Al re-escanear, si el canónico sigue existiendo en disco, la entrada se conserva. Si ya no existe, `cleanup_stale` lo elimina y el próximo archivo con ese hash que aparezca se convertirá en el nuevo canónico.

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

# Limpieza completa: borrar sueltos, borrar comprimidos redundantes, preferir el comprimido
media-index dupes --delete --aggressive --prefer-archive

# Watch con BD personalizada y debounce largo (útil en redes lentas)
media-index -d /nas/index.db watch /nas/media --debounce 10

# Re-indexar desde cero
media-index clear --force && media-index scan ./fotos

# Exportar y hacer backup del índice
media-index export --output "index_$(date +%Y%m%d).json"
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
└── parsers/
    ├── mod.rs       — Dispatcher: elige el parser según el tipo
    ├── audio.rs     — Tags con lofty
    ├── image.rs     — Dimensiones + EXIF con image + kamadak-exif
    ├── video.rs     — Metadatos vía ffprobe (subproceso)
    └── print3d.rs   — Geometría de STL/OBJ/3MF
```
