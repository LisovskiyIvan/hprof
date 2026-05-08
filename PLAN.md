# HProf Implementation Plan

## Overview

CLI + Web UI tool for analyzing V8 memory profiles:
- `.heapsnapshot` — full heap snapshot
- `.heapprofile` — sampling heap profile
- `.heaptimeline` — heap allocation timeline

## Architecture

```
packages/
├── core/      # Парсинг и анализ (нет зависимостей на FS кроме bun/node)
├── cli/       # CLI утилита hprof
└── ui/        # HTTP-сервер + SPA фронтенд
```

---

## Phase 1: `@hprof/core` — Парсеры

### 1.1 Общие утилиты (`src/index.ts`)
- [x] `detectProfileType(filePath)` — определение типа по расширению
- [x] `formatBytes(bytes)` — форматирование размера
- [ ] `parseSnapshotMeta(filePath)` — извлечение JSON-заголовка heapsnapshot/heaptimeline (уже есть в `analyze-memory-profiles.mjs:108-169`)
- [ ] `StreamingJsonParser` — потоковый парсер JSON с state machine (seek nodes → parse numbers → seek strings → parse strings)

### 1.2 Heapprofile parser (`src/heapprofile.ts`)
Самый простой формат — обычный JSON целиком в память.

- [ ] `parseHeapProfile(filePath)` → `HeapProfileResult`
  - `JSON.parse` всего файла
  - Рекурсивный обход `head` дерева
- [ ] `summarizeHeapProfile(data, options)` → `HeapProfileSummary`
  - Агрегация по `byFrame`, `byUrl`, `byFunction`
  - Поддержка `--top` и `--filter`
- [ ] `flattenToCallFrames(data)` — плоская таблица фреймов для UI

**Референс:** `analyze-memory-profiles.mjs:59-106`

### 1.3 Heapsnapshot parser (`src/heapsnapshot.ts`)
Самый тяжёлый формат — может быть гигабайты.

- [ ] `streamHeapSnapshotSummary(filePath, options)` → `HeapSnapshotSummary`
  - Streaming парсинг через `fs.createReadStream`
  - State machine: `seekNodes` → `parseNodes` → `seekStrings` → `parseStrings`
  - Агрегация top-N по имени и типу (не грузить всё в память)
- [ ] `parseHeapSnapshot(filePath)` → `HeapSnapshotResult`
  - Полный парсинг для UI (постраничная загрузка)
  - Десериализация nodes[], edges[], strings[] в объекты
- [ ] `buildRetainedSize(snapshot)` — расчёт retained size (BFS/DFS от GC roots)

**Референс:** `analyze-memory-profiles.mjs:108-340`

### 1.4 Heaptimeline parser (`src/heaptimeline.ts`)
Похож на heapsnapshot, но с массивом `timeline` записей.

- [ ] `parseHeapTimeline(filePath)` → `HeapTimelineResult`
- [ ] `streamHeapTimelineSummary(filePath, options)` → `HeapTimelineSummary`
  - Агрегация allocated/freed по типам
  - Временные интервалы для графика

### 1.5 Структуры данных для UI API
- [ ] Определить типы для API-ответов (summary, details, comparison)
- [ ] `serializeSummary()` — конвертация Map → JSON-сериализуемый объект

---

## Phase 2: `@hprof/cli` — CLI

### 2.1 Базовая структура (`src/cli.ts`)
- [x] Парсинг аргументов: `hprof [command] [options] <file>`
- [x] Команды: `analyze` (default), `ui`, `help`

### 2.2 Команда `analyze`
- [ ] `hprof file.heapprofile` — вывод top фреймов/URL/функций
- [ ] `hprof file.heapsnapshot` — вывод top nodes by name/type + мета
- [ ] `hprof file.heaptimeline` — вывод allocation summary
- [ ] Поддержка `--top N`, `--filter regex`
- [ ] Цветной вывод (chalk/colors) или ANSI напрямую
- [ ] Таблицы в терминале (форматирование колонок)

### 2.3 CLI UX
- [ ] Прогресс-бар для больших файлов
- [ ] Автоопределение типа файла
- [ ] Обработка ошибок (битые файлы, не тот формат)
- [ ] `--json` флаг для машиночитаемого вывода

---

## Phase 3: `@hprof/ui` — Web UI

### 3.1 Сервер (`src/server/`)

- [ ] Bun HTTP сервер на порту 3000 (configurable)
- [ ] Auto-open браузер (`--open`)
- [ ] Static file serving для SPA

**API endpoints:**

```
GET  /api/profile/meta              → метаданные файла (тип, размер, node_count и тд)
GET  /api/profile/summary           → агрегированная сводка (top-N)
GET  /api/profile/nodes?type=&page= → пагинация по nodes (heapsnapshot)
GET  /api/profile/edges?nodeId=     → edges для конкретного node
GET  /api/profile/tree              → дерево вызовов (heapprofile)
GET  /api/profile/timeline          → timeline данные (heaptimeline)
GET  /api/profile/retained          → retained size top
GET  /api/profile/search?q=         → поиск по строкам/именам
```

- [ ] Ленивая загрузка — парсинг по запросу
- [ ] Кеширование распарсенных данных в памяти
- [ ] Потоковая передача больших ответов (chunked)

### 3.2 Фронтенд (`src/client/`)

Минимальный SPA без фреймворка (vanilla + htm/preact или solid):

- [ ] **Обзорная страница (Summary)**
  - Общая статистика: total size, node count, edge count
  - Pie chart / bar chart по типам nodes
  - Top-20 nodes по self size
  - Top-20 nodes по retained size

- [ ] **Таблица Nodes (Heapsnapshot)**
  - Виртуализированная таблица (миллионы строк)
  - Колонки: type, name, self_size, retained_size, edge_count
  - Фильтры по типу, поиск по имени
  - Сортировка по всем колонкам
  - Пагинация / infinite scroll

- [ ] **Дерево вызовов (Heapprofile)**
  - Treemap / Flamechart визуализация
  - Expandable tree view
  - Подсветка hot paths
  - Фильтрация по URL/function

- [ ] **Timeline (Heaptimeline)**
  - График аллокаций по времени
  - Zoom / pan
  - Стекирование по типам

- [ ] **Retained Size View**
  - Treemap retained объектов
  - Path от GC root до объекта
  - Dominator tree

- [ ] **Поиск**
  - Полнотекстовый поиск по строкам/именам
  - Переход к найденному объекту

### 3.3 UI технологии (выбор)
- [ ] Рендеринг: **Preact + htm** (или vanilla TS) — минимальный бандл
- [ ] Таблица: кастомная виртуализация (или clusterize.js / tanstack-virtual)
- [ ] Графики: **Chart.js** или **uPlot** (легковесные)
- [ ] Treemap: **d3-hierarchy** или кастомный canvas
- [ ] Стили: CSS modules или Tailwind

---

## Phase 4: Полировка

- [ ] Тесты для парсеров (unit + snapshot тесты на реальных файлах)
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
