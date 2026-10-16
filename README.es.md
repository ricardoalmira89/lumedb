# NaviDog

[English](README.md) · **Español**

Cliente gráfico de bases de datos escrito en Rust, con la disposición y el flujo de trabajo de Navicat.
Motores: **PostgreSQL**, **MySQL / MariaDB** y **SQLite**.

## Reglas de sala limpia (obligatorias)

NaviDog es una implementación independiente. Para que siga siéndolo:

1. **No se descompila ni se desensambla Navicat**, ni se copia su código, sus recursos (iconos, textos, traducciones) ni su marca.
2. Sí se permite **usar Navicat con normalidad y observar su comportamiento** (Directiva 2009/24/CE, art. 5.3) y reimplementar funcionalidades, que no están protegidas por derechos de autor (TJUE C-406/10, *SAS Institute*).
3. La interoperabilidad con ficheros de Navicat (p. ej. importar conexiones `.ncx`) se implementa a partir de **documentación pública** y de ficheros de ejemplo propios, nunca a partir del binario.

## Funciones

**Conexiones**
- PostgreSQL (una conexión por base de datos, como Navicat), MySQL/MariaDB y SQLite.
- SSL (CA propia, verificación opcional) y túnel SSH (contraseña o clave pública).
- Grupos (carpetas) y colores; duplicar, editar y probar la conexión.
- Importación de conexiones de Navicat (`.ncx`), con contraseñas y SSH.

**Navegación**
- Árbol conexión → base de datos → esquema → tablas, vistas, funciones, procedimientos, disparadores, secuencias, eventos, consultas guardadas y copias de seguridad; búsqueda de objetos.
- Pestaña «Objetos» con filas, tamaño, motor, fechas y comentario; panel de información con pestañas General y DDL.
- Vista de diagrama ER con minimapa, organización automática sin cruces de relaciones y enlaces curvos de columna a columna.

**Datos**
- Rejilla editable: editar celdas, añadir/borrar filas, NULL / cadena vacía, editor de texto largo, aplicar o descartar cambios.
- Transacciones manuales (iniciar, confirmar, revertir), filtro WHERE, orden por columna, paginación con total, vista de formulario.

**Consultas**
- Resaltado de sintaxis, números de línea, autocompletado (palabras clave, tablas, columnas, `alias.columna`).
- Ejecutar todo, la selección o la sentencia actual; detener la consulta; varios resultados; Explicar; formatear.
- Gráficos (barras, líneas, área, dispersión, circular), generador visual de consultas, fragmentos de código.
- Consultas guardadas, historial y exportación de resultados.

**Diseño de objetos**
- Diseñador de tablas: campos, índices, claves foráneas, opciones y vista previa SQL (CREATE/ALTER; reconstrucción en SQLite).
- Editor de vistas, funciones, procedimientos, disparadores, eventos y secuencias, con plantillas por motor.

**Herramientas**
- Importar (CSV, TXT, JSON, Excel) con mapeo de columnas; exportar (CSV, TXT, JSON, XML, HTML, Markdown, SQL, Excel).
- Volcar a SQL y ejecutar archivos SQL; copias de seguridad comprimidas con restauración.
- Transferencia de datos entre conexiones (también entre motores distintos), sincronización de datos y de estructura.
- Usuarios y privilegios (servidor y por base de datos/esquema), monitor del servidor (procesos, variables, estado).
- Buscar en la base de datos, generación de datos de prueba, modelo ER, consola, automatización programada.
- Generación de SQL con IA (Claude u OpenAI): escribe un comentario y pulsa Ctrl+Enter.
- Interfaz en inglés y español (Ver → Idioma); tema claro y oscuro.

## Diferencias con Navicat (pendiente)

- Motores que faltan: SQL Server, Oracle, MongoDB, Redis y otros.
- El modelo ER es inverso (diagrama de la base existente + exportar SQL); no es un diseñador lógico/físico completo.
- La automatización se ejecuta mientras NaviDog está abierto o desde cron (`navidog --run-job "nombre"`); no envía correos.
- Sin depurador de funciones, informes, ni sincronización en la nube.
- Las contraseñas se guardan en texto plano en `~/.config/navidog/connections.json`.

## Uso

```sh
cargo run --release
cargo test                        # SQLite
NAVIDOG_TEST_LIVE=1 cargo test    # además PostgreSQL (puerto 55432) y MySQL (33306), contraseña «navidog»
```

Contenedores para las pruebas en vivo:

```sh
docker run -d --name navidog-test-pg -e POSTGRES_PASSWORD=navidog -p 127.0.0.1:55432:5432 postgres:16-alpine
docker run -d --name navidog-test-mysql -e MYSQL_ROOT_PASSWORD=navidog -p 127.0.0.1:33306:3306 mysql:latest
```

## Traducciones

El inglés es el idioma base: todos los textos de la interfaz se escriben en inglés dentro de `tr("…")`
(o `trf!("… {name} …", name = valor)`). Las traducciones están en `crates/navidog-core/i18n/<idioma>.json`,
un mapa del texto en inglés al texto traducido; si falta una entrada se muestra el inglés.
Para añadir un idioma, crea su catálogo y añade una variante a `Lang` en `crates/navidog-core/src/i18n.rs`.

## Estructura

- `crates/navidog-core`: conexiones (SSL/SSH), drivers, metadatos, DDL, edición, SQL, exportación/importación, volcado, sincronización, administración y automatización.
- `crates/navidog-app`: interfaz (egui). `app.rs` reparte las tareas a un hilo por conexión; cada pestaña o ventana es una `View` en `views/`.
