# HProf Implementation Plan

## Overview

CLI + Web UI tool for analyzing V8 memory profiles:

- `.heapsnapshot` — full heap snapshot
- `.heapprofile` — sampling heap profile
- `.heaptimeline` — heap allocation timeline

## Architecture

```
crates/
├── hprof-core/   # Парсинг и анализ (Rust)
├── hprof-cli/    # CLI утилита hprof (Rust, нативно против hprof-core)
└── hprof-c/      # cdylib FFI-мост (для будущего UI)
packages/
├── core/         # TS FFI-биндинги (UI отложен)
└── ui/           # HTTP-сервер + SPA фронтенд (отложен)
```

CLI полностью на Rust — без FFI-прослойки: `analyze`, `diff`, `dot`,
`list`, `inspect`, `--retained`. UI-часть (packages/ui + packages/core +
hprof-c) сохранена, но не подключена — вернётся позже.

---

## Phase 1: `@hprof/core` — Парсеры

### 1.1 Общие утилиты (`src/index.ts`)

- [x] `detectProfileType(filePath)` — определение типа по расширению
- [x] `formatBytes(bytes)` — форматирование размера
- [x] `parseSnapshotMeta(filePath)` — извлечение JSON-заголовка heapsnapshot/heaptimeline (уже есть в `analyze-memory-profiles.mjs:108-169`)
- [x] `StreamingJsonParser` — потоковый парсер JSON с state machine (seek nodes → parse numbers → seek strings → parse strings)

### 1.2 Heapprofile parser (`src/heapprofile.ts`)

Самый простой формат — обычный JSON целиком в память.

- [x] `parseHeapProfile(filePath)` → `HeapProfileResult`
  - `JSON.parse` всего файла
  - Рекурсивный обход `head` дерева
- [x] `summarizeHeapProfile(data, options)` → `HeapProfileSummary`
  - Агрегация по `byFrame`, `byUrl`, `byFunction`
  - Поддержка `--top` и `--filter`
- [x] `flattenToCallFrames(data)` — плоская таблица фреймов для UI

**Референс:** `analyze-memory-profiles.mjs:59-106`

### 1.3 Heapsnapshot parser (`src/heapsnapshot.ts`)

Самый тяжёлый формат — может быть гигабайты.

- [x] `streamHeapSnapshotSummary(filePath, options)` → `HeapSnapshotSummary`
  - Streaming парсинг через `fs.createReadStream`
  - State machine: `seekNodes` → `parseNodes` → `seekStrings` → `parseStrings`
  - Агрегация top-N по имени и типу (не грузить всё в память)
- [x] `parseHeapSnapshot(filePath)` → `HeapSnapshotResult`
  - Полный парсинг для UI (постраничная загрузка)
  - Десериализация nodes[], edges[], strings[] в объекты
- [x] `buildRetainedSize(snapshot)` — расчёт retained size (BFS/DFS от GC roots)

**Референс:** `analyze-memory-profiles.mjs:108-340`

### 1.4 Heaptimeline parser (`src/heaptimeline.ts`)

Похож на heapsnapshot, но с массивом `timeline` записей.

- [x] `parseHeapTimeline(filePath)` → `HeapTimelineResult`
- [x] `streamHeapTimelineSummary(filePath, options)` → `HeapTimelineSummary`
  - Агрегация allocated/freed по типам
  - Временные интервалы для графика

### 1.5 Структуры данных для UI API

- [x] Определить типы для API-ответов (summary, details, comparison)
- [x] `serializeSummary()` — конвертация Map → JSON-сериализуемый объект

---

## Phase 2: CLI (портирован в `crates/hprof-cli` на Rust)

> Первоначальный Bun/TS CLI (`packages/cli`) удалён; команды и JSON-контракты
> портированы один-в-один в нативный Rust CLI (`cargo build --release` →
> `target/release/hprof`). Пункты ниже отражают исходный план.

### 2.1 Базовая структура (`src/cli.ts`)

- [x] Парсинг аргументов: `hprof [command] [options] <file>`
- [x] Команды: `analyze` (default), `ui`, `help`

### 2.2 Команда `analyze`

- [x] `hprof file.heapprofile` — вывод top фреймов/URL/функций
- [x] `hprof file.heapsnapshot` — вывод top nodes by name/type + мета
- [x] `hprof file.heaptimeline` — вывод allocation summary
- [x] Поддержка `--top N`, `--filter regex`
- [x] Цветной вывод (chalk/colors) или ANSI напрямую
- [x] Таблицы в терминале (форматирование колонок)

### 2.3 CLI UX

- [x] Прогресс-бар для больших файлов
- [x] Автоопределение типа файла
- [x] Обработка ошибок (битые файлы, не тот формат)
- [x] `--json` флаг для машиночитаемого вывода

---

## Phase 3: `@hprof/ui` — Web UI

### 3.1 Сервер (`src/server/`)

- [x] Bun HTTP сервер на порту 3000 (configurable)
- [x] Auto-open браузер (`--open`)
- [x] Static file serving для SPA

**API endpoints:**

```
GET  /api/profiles                    → список загруженных профилей
GET  /api/profile/:id/meta           → метаданные файла (тип, размер, node_count и тд)
GET  /api/profile/:id/summary        → агрегированная сводка (top-N)
GET  /api/profile/:id/nodes?type=&page= → пагинация по nodes (heapsnapshot)
GET  /api/profile/:id/edges?nodeId=  → edges для конкретного node
GET  /api/profile/:id/tree           → дерево вызовов (heapprofile)
GET  /api/profile/:id/timeline       → timeline данные (heaptimeline)
GET  /api/profile/:id/search?q=      → поиск по строкам/именам
```

- [x] Ленивая загрузка — парсинг по запросу
- [x] Кеширование распарсенных данных в памяти
- [ ] Потоковая передача больших ответов (chunked)

### 3.2 Фронтенд (`src/client/`)

Минимальный SPA React:

- [x] **Обзорная страница (Summary)**
  - Общая статистика: total size, node count, edge count
  - Pie chart / bar chart по типам nodes
  - Top-20 nodes по self size

- [x] **Таблица Nodes (Heapsnapshot)**
  - Пагинированная таблица
  - Колонки: type, name, self_size, edge_count
  - Фильтры по типу, поиск по имени
  - Сортировка по всем колонкам

- [x] **Дерево вызовов (Heapprofile)**
  - Expandable tree view
  - Подсветка hot paths

- [ ] **Timeline (Heaptimeline)**
  - График аллокаций по времени
  - Zoom / pan
  - Стекирование по типам

- [x] **Retained Size View**
  - Top-N таблица retained объектов
  - Dominator tree (через buildRetainedSize)

- [x] **Поиск**
  - Полнотекстовый поиск по строкам/именам
  - Переход к найденному объекту

### 3.3 UI технологии (выбор)

- [ ] Рендеринг: **Preact + htm** (или vanilla TS) — минимальный бандл
- [ ] Таблица: кастомная виртуализация (или clusterize.js / tanstack-virtual)
- [x] Графики: **Chart.js** или **uPlot** (легковесные)
- [ ] Treemap: **d3-hierarchy** или кастомный canvas
- [x] Стили: CSS modules или Tailwind

---

## Phase 4: Полировка

- [x] Тесты для парсеров (unit + snapshot тесты на реальных файлах)
- [ ] Benchmark на больших файлах (500MB+ heapsnapshot)
- [ ] Документация README
- [ ] npm publish: `hprof` как глобальный пакет
- [ ] CI: lint, typecheck, test

---

## Priority Order

1. **Core: heapprofile parser** — самый простой, начало с него
2. **Core: heapsnapshot streaming parser** — основной use case
3. **CLI: analyze** — минимальный рабочий инструмент
4. **UI: server + summary page** — первый экран
5. **UI: nodes table** — основная функциональность
6. **Core: heaptimeline parser** — третий формат
7. **UI: flamechart/treemap** — визуализации
8. **UI: retained size + search** — продвинутые фичи

---

## File Layout (final)

```
hprof/
├── packages/
│   ├── core/
│   │   ├── package.json
│   │   └── src/
│   │       ├── index.ts          # re-exports + utils
│   │       ├── heapprofile.ts    # heapprofile parser
│   │       ├── heapsnapshot.ts   # heapsnapshot parser (streaming)
│   │       └── heaptimeline.ts   # heaptimeline parser
│   ├── cli/
│   │   ├── package.json
│   │   └── src/
│   │       └── cli.ts            # hprof entry point
│   └── ui/
│       ├── package.json
│       └── src/
│           ├── server/
│           │   └── index.ts      # Bun HTTP server + API routes
│           └── client/
│               ├── index.html
│               ├── app.ts
│               ├── components/
│               │   ├── Summary.ts
│               │   ├── NodesTable.ts
│               │   ├── Flamechart.ts
│               │   ├── Timeline.ts
│               │   └── Search.ts
│               └── styles/
│                   └── main.css
├── package.json                  # workspaces root
├── tsconfig.json
├── PLAN.md
└── snapshots/                    # test fixtures
```
