"use client"

import { useEffect, useState } from "react"
import { invoke } from "@tauri-apps/api/core"
import { Switch } from "./ui/switch"

interface ObsidianConfig {
  enabled: boolean
  vault_path: string
}

export function ObsidianExportSettings() {
  const [config, setConfig] = useState<ObsidianConfig | null>(null)

  useEffect(() => {
    invoke<ObsidianConfig>("get_obsidian_export_config")
      .then(setConfig)
      .catch((e) => console.error("Failed to load Obsidian config:", e))
  }, [])

  const save = async (next: ObsidianConfig) => {
    setConfig(next)
    try {
      await invoke("set_obsidian_export_config", { config: next })
    } catch (e) {
      console.error("Failed to save Obsidian config:", e)
    }
  }

  if (!config) return null

  return (
    <div className="bg-white rounded-lg border border-gray-200 p-6 shadow-sm">
      <div className="flex items-center justify-between">
        <div>
          <h3 className="text-lg font-semibold text-gray-900 mb-2">Save summaries to Obsidian</h3>
          <p className="text-sm text-gray-600">Writes each generated summary (no transcript) as a note in your vault</p>
        </div>
        <Switch checked={config.enabled} onCheckedChange={(enabled) => save({ ...config, enabled })} />
      </div>
      <input
        type="text"
        value={config.vault_path}
        onChange={(e) => setConfig({ ...config, vault_path: e.target.value })}
        onBlur={() => save(config)}
        className="mt-4 w-full px-3 py-2 text-sm font-mono border border-gray-300 rounded-md"
        aria-label="Obsidian vault path"
      />
    </div>
  )
}
