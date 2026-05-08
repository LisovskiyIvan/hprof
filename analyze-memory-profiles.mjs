#!/usr/bin/env node

import fs from 'fs';
import path from 'path';

function parseArgs(argv) {
  const args = {
    files: [],
    top: 30,
    filter: null,
  };

  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === '--top') {
      args.top = Number(argv[++i] ?? args.top);
    } else if (arg === '--filter') {
      args.filter = argv[++i] ?? null;
    } else {
      args.files.push(arg);
    }
  }

  return args;
}

function formatBytes(bytes) {
  if (!Number.isFinite(bytes)) return String(bytes);
  const units = ['B', 'KB', 'MB', 'GB'];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(value >= 100 ? 0 : value >= 10 ? 1 : 2)} ${units[unit]}`;
}

function printSection(title, rows) {
  console.log(`\n${title}`);
  if (!rows.length) {
    console.log('  (empty)');
    return;
  }

  for (const row of rows) {
    const parts = [];
    for (const [key, value] of Object.entries(row)) {
      parts.push(`${key}=${value}`);
    }
    console.log(`  ${parts.join(' | ')}`);
  }
}

function fileExt(filePath) {
  return path.extname(filePath).toLowerCase();
}

function analyzeHeapProfile(filePath, { top, filter }) {
  const data = JSON.parse(fs.readFileSync(filePath, 'utf8'));
  const byFrame = new Map();
  const byUrl = new Map();
  const byFunction = new Map();
  const filterRe = filter ? new RegExp(filter, 'i') : null;

  function add(map, key, size) {
    const value = map.get(key) ?? 0;
    map.set(key, value + size);
  }

  function walk(node, stack) {
    const cf = node.callFrame || {};
    const fn = cf.functionName || '(anonymous)';
    const url = cf.url || '<no-url>';
    const line = (cf.lineNumber ?? -1) + 1;
    const frame = `${fn} @ ${url}:${line}`;
    const nextStack = stack.concat(frame);
    const selfSize = node.selfSize || 0;

    if (selfSize > 0) {
      const hay = `${frame}\n${nextStack.join('\n')}`;
      if (!filterRe || filterRe.test(hay)) {
        add(byFrame, frame, selfSize);
        add(byUrl, url, selfSize);
        add(byFunction, fn, selfSize);
      }
    }

    for (const child of node.children || []) {
      walk(child, nextStack);
    }
  }

  walk(data.head, []);

  const toRows = (map) =>
    [...map.entries()]
      .sort((a, b) => b[1] - a[1])
      .slice(0, top)
      .map(([key, size]) => ({ size: formatBytes(size), key }));

  console.log(`\n=== ${path.basename(filePath)} ===`);
  printSection('Top Frames', toRows(byFrame));
  printSection('Top URLs', toRows(byUrl));
  printSection('Top Functions', toRows(byFunction));
}

function buildSnapshotMeta(filePath) {
  const fd = fs.openSync(filePath, 'r');
  try {
    let chunkSize = 2 * 1024 * 1024;
    const maxChunkSize = 64 * 1024 * 1024;

    while (chunkSize <= maxChunkSize) {
      const buffer = Buffer.alloc(chunkSize);
      const bytesRead = fs.readSync(fd, buffer, 0, chunkSize, 0);
      const prefix = buffer.subarray(0, bytesRead).toString('utf8');
      const snapshotMarker = '"snapshot":';
      const snapshotIndex = prefix.indexOf(snapshotMarker);
      const nodesIndex = prefix.indexOf('"nodes":[');
      if (snapshotIndex !== -1 && nodesIndex !== -1) {
        const objectStart = prefix.indexOf('{', snapshotIndex + snapshotMarker.length);
        if (objectStart !== -1 && objectStart < nodesIndex) {
          let depth = 0;
          let inString = false;
          let escaped = false;
          for (let i = objectStart; i < prefix.length; i++) {
            const ch = prefix[i];
            if (inString) {
              if (escaped) {
                escaped = false;
              } else if (ch === '\\') {
                escaped = true;
              } else if (ch === '"') {
                inString = false;
              }
              continue;
            }

            if (ch === '"') {
              inString = true;
              continue;
            }

            if (ch === '{') {
              depth += 1;
            } else if (ch === '}') {
              depth -= 1;
              if (depth === 0) {
                return JSON.parse(prefix.slice(objectStart, i + 1));
              }
            }
          }
        }
      }

      if (!prefix.includes('"nodes":[')) {
        chunkSize *= 2;
        continue;
      }

      throw new Error('Cannot parse snapshot header even though nodes marker exists');
    }

    throw new Error('Cannot parse snapshot header: prefix limit exceeded');
  } finally {
    fs.closeSync(fd);
  }
}

async function analyzeHeapSnapshotLike(filePath, { top, filter }) {
  const snapshot = buildSnapshotMeta(filePath);
  const nodeFields = snapshot.meta.node_fields;
  const nodeTypes = snapshot.meta.node_types[0];
  const nodeFieldCount = nodeFields.length;
  const typeOffset = nodeFields.indexOf('type');
  const nameOffset = nodeFields.indexOf('name');
  const selfSizeOffset = nodeFields.indexOf('self_size');
  const filterRe = filter ? new RegExp(filter, 'i') : null;

  if (typeOffset < 0 || nameOffset < 0 || selfSizeOffset < 0) {
    throw new Error('Unsupported node field layout');
  }

  const byNameIndex = new Map();
  const byTypeIndex = new Map();

  let mode = 'seekNodes';
  let record = [];
  let currentNumber = '';
  let currentString = '';
  let stringEscape = false;
  let stringIndex = -1;
  let currentChar = '';

  const stream = fs.createReadStream(filePath, { encoding: 'utf8' });

  for await (const chunk of stream) {
    let i = 0;
    while (i < chunk.length) {
      if (mode === 'seekNodes') {
        const idx = chunk.indexOf('"nodes":[', i);
        if (idx === -1) {
          break;
        }
        i = idx + '"nodes":['.length;
        mode = 'parseNodes';
        continue;
      }

      if (mode === 'parseNodes') {
        currentChar = chunk[i];
        if (currentChar >= '0' && currentChar <= '9') {
          currentNumber += currentChar;
        } else if (currentChar === '-') {
          currentNumber += currentChar;
        } else if (currentChar === ',' || currentChar === ']') {
          if (currentNumber) {
            record.push(Number(currentNumber));
            currentNumber = '';
          }

          if (record.length === nodeFieldCount) {
            const typeIndex = record[typeOffset];
            const nameIndex = record[nameOffset];
            const selfSize = record[selfSizeOffset];

            if (selfSize > 0) {
              const prev = byNameIndex.get(nameIndex) ?? {
                size: 0,
                count: 0,
                typeIndex,
              };
              prev.size += selfSize;
              prev.count += 1;
              byNameIndex.set(nameIndex, prev);

              const typePrev = byTypeIndex.get(typeIndex) ?? { size: 0, count: 0 };
              typePrev.size += selfSize;
              typePrev.count += 1;
              byTypeIndex.set(typeIndex, typePrev);
            }

            record = [];
          }

          if (currentChar === ']') {
            mode = 'seekStrings';
          }
        }

        i += 1;
        continue;
      }

      if (mode === 'seekStrings') {
        const idx = chunk.indexOf('"strings":[', i);
        if (idx === -1) {
          break;
        }
        i = idx + '"strings":['.length;
        mode = 'parseStrings';

        const topNameIndices = [...byNameIndex.entries()]
          .sort((a, b) => b[1].size - a[1].size)
          .slice(0, top * 5)
          .map(([index]) => index);
        stream.topNameIndices = new Set(topNameIndices);
        stream.topNames = new Map();
        continue;
      }

      if (mode === 'parseStrings') {
        currentChar = chunk[i];
        if (currentString === '' && currentChar === '"') {
          currentString = '"';
        } else if (currentString !== '') {
          currentString += currentChar;

          if (stringEscape) {
            stringEscape = false;
          } else if (currentChar === '\\') {
            stringEscape = true;
          } else if (currentChar === '"') {
            stringIndex += 1;
            if (stream.topNameIndices.has(stringIndex)) {
              const value = JSON.parse(currentString);
              stream.topNames.set(stringIndex, value);
            }
            currentString = '';
          }
        } else if (currentChar === ']') {
          mode = 'done';
          break;
        }

        i += 1;
        continue;
      }

      if (mode === 'done') {
        break;
      }
    }
  }

  const rows = [...byNameIndex.entries()]
    .map(([nameIndex, info]) => ({
      nameIndex,
      name: stream.topNames.get(nameIndex) ?? `<string#${nameIndex}>`,
      type: nodeTypes[info.typeIndex] ?? String(info.typeIndex),
      size: info.size,
      count: info.count,
    }))
    .filter((row) => !filterRe || filterRe.test(`${row.name} ${row.type}`))
    .sort((a, b) => b.size - a.size)
    .slice(0, top)
    .map((row) => ({
      size: formatBytes(row.size),
      count: row.count,
      type: row.type,
      name: row.name,
    }));

  const typeRows = [...byTypeIndex.entries()]
    .sort((a, b) => b[1].size - a[1].size)
    .slice(0, top)
    .map(([typeIndex, info]) => ({
      size: formatBytes(info.size),
      count: info.count,
      type: nodeTypes[typeIndex] ?? String(typeIndex),
    }));

  console.log(`\n=== ${path.basename(filePath)} ===`);
  console.log(
    `node_count=${snapshot.node_count} | edge_count=${snapshot.edge_count} | extra_native_bytes=${formatBytes(snapshot.extra_native_bytes ?? 0)}`
  );
  printSection('Top Node Names By Self Size', rows);
  printSection('Top Node Types By Self Size', typeRows);
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  if (args.files.length === 0) {
    console.error(
      'Usage: node scripts/analyze-memory-profiles.mjs [--top 30] [--filter regex] <file...>'
    );
    process.exit(1);
  }

  for (const filePath of args.files) {
    const ext = fileExt(filePath);
    if (ext === '.heapprofile') {
      analyzeHeapProfile(filePath, args);
    } else if (ext === '.heapsnapshot' || ext === '.heaptimeline') {
      await analyzeHeapSnapshotLike(filePath, args);
    } else {
      console.warn(`Skip unsupported file: ${filePath}`);
    }
  }
}

await main();
