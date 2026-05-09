import { useEffect, useState } from 'react'

interface CallFrame {
  functionName: string
  url: string
  lineNumber: number
  columnNumber: number
}

interface TreeNode {
  callFrame: CallFrame
  selfSize: number
  children: TreeNode[]
}

function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes === 0) return ''
  const units = ['B', 'KB', 'MB', 'GB']
  let value = bytes
  let unit = 0
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024
    unit += 1
  }
  return `${value.toFixed(value >= 100 ? 0 : value >= 10 ? 1 : 2)} ${units[unit]}`
}

export default function TreeView({ base }: { base: string }) {
  const [tree, setTree] = useState<TreeNode | null>(null)

  useEffect(() => {
    fetch(`${base}/tree`)
      .then((r) => r.json())
      .then(setTree)
  }, [base])

  if (!tree) return <p className="text-gray-500">Loading tree...</p>

  const totalSize = computeTotalSize(tree)

  return (
    <div className="space-y-2">
      <p className="text-sm text-gray-400">
        Total sampled: <span className="text-white font-semibold">{formatBytes(totalSize)}</span>
      </p>
      <div className="bg-gray-900 rounded-lg overflow-hidden">
        <TreeRow node={tree} totalSize={totalSize} depth={0} />
      </div>
    </div>
  )
}

function computeTotalSize(node: TreeNode): number {
  let size = node.selfSize
  for (const child of node.children) {
    size += computeTotalSize(child)
  }
  return size
}

function TreeRow({ node, totalSize, depth }: { node: TreeNode; totalSize: number; depth: number }) {
  const [expanded, setExpanded] = useState(depth < 2)
  const fn = node.callFrame.functionName || '(anonymous)'
  const url = node.callFrame.url || ''
  const line = node.callFrame.lineNumber + 1
  const childTotal = computeTotalSize(node)
  const pct = totalSize > 0 ? (childTotal / totalSize) * 100 : 0

  return (
    <div>
      <button
        className="w-full text-left px-3 py-1.5 hover:bg-gray-800/50 flex items-center gap-2 text-sm"
        style={{ paddingLeft: `${depth * 16 + 12}px` }}
        onClick={() => setExpanded(!expanded)}
      >
        <span className="text-gray-600 text-xs w-3">
          {node.children.length > 0 ? (expanded ? '▼' : '▶') : ' '}
        </span>
        <span
          className={`font-mono text-xs truncate flex-1 ${node.selfSize > 0 ? 'text-indigo-400' : 'text-gray-300'}`}
        >
          {fn}
        </span>
        {url && (
          <span className="text-gray-600 text-xs truncate max-w-xs" title={`${url}:${line}`}>
            {url.split('/').pop()}:{line}
          </span>
        )}
        {node.selfSize > 0 && (
          <span className="text-xs text-gray-400 whitespace-nowrap">
            {formatBytes(node.selfSize)}
          </span>
        )}
        {pct >= 1 && (
          <div className="w-16 h-1.5 bg-gray-800 rounded-full overflow-hidden">
            <div
              className="h-full bg-indigo-500 rounded-full"
              style={{ width: `${Math.min(pct, 100)}%` }}
            />
          </div>
        )}
      </button>
      {expanded &&
        node.children.map((child, i) => (
          <TreeRow
            key={`${depth}-${i}-${child.callFrame.functionName}`}
            node={child}
            totalSize={totalSize}
            depth={depth + 1}
          />
        ))}
    </div>
  )
}
