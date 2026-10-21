// Pure tool-profile registry. No Electron, no I/O.
// Which names a turn advertises is decided here. Registration stays complete
// for external clients; later PRs filter at advertise time.
// See docs/design-mcp-tool-profiles.md.

export const TOOL_PROFILES = ['browse', 'reverse', 'probe'] as const
export type ToolProfile = (typeof TOOL_PROFILES)[number]

export type ToolBucket = 'browse' | 'reverse' | 'probe' | 'hidden'

/** core: every in-app profile. hidden: none. Otherwise the union of the listed profiles. */
export const TOOL_BUCKETS: Record<string, readonly ToolBucket[] | 'core'> = {
  // core (27) — existing 22 + tool_profile_set + dialog answer/history + downloads
  tool_profile_set: 'core',
  browser_navigate: 'core',
  browser_snapshot: 'core',
  browser_click: 'core',
  browser_type: 'core',
  browser_scroll: 'core',
  browser_press_key: 'core',
  browser_click_selector: 'core',
  browser_type_selector: 'core',
  browser_evaluate: 'core',
  dom_extract: 'core',
  browser_wait_for: 'core',
  list_requests: 'core',
  get_request: 'core',
  tool_result_more: 'core',
  console_logs: 'core',
  console_exceptions: 'core',
  scope_status: 'core',
  scope_set: 'core',
  scope_add: 'core',
  scope_clear: 'core',
  finding_add: 'core',
  finding_list: 'core',
  // Pages pop dialogs and downloads in any profile; the agent must be able to steer them.
  browser_handle_dialog: 'core',
  dialog_history: 'core',
  list_downloads: 'core',
  wait_for_download: 'core',

  // browse only (34)
  browser_hover: ['browse'],
  browser_scroll_element: ['browse'],
  browser_draw: ['browse'],
  browser_paste: ['browse'],
  browser_tabs: ['browse'],
  browser_file_upload: ['browse'],
  browser_history: ['browse'],
  browser_click_modified: ['browse'],
  browser_drag: ['browse'],
  browser_select_option: ['browse'],
  browser_hover_selector: ['browse'],
  browser_screenshot: ['browse'],
  set_viewport: ['browse'],
  dialog_set_auto_dismiss: ['browse'],
  dialog_clear_history: ['browse'],
  dialog_inject_override: ['browse'],
  dom_set_attr: ['browse'],
  dom_set_style: ['browse'],
  dom_set_text: ['browse'],
  dom_remove: ['browse'],
  vision_judge: ['browse'],
  console_clear: ['browse'],
  local_storage_get: ['browse'],
  local_storage_set: ['browse'],
  local_storage_delete: ['browse'],
  local_storage_clear: ['browse'],
  session_storage_get: ['browse'],
  session_storage_set: ['browse'],
  session_storage_delete: ['browse'],
  session_storage_clear: ['browse'],
  sw_list: ['browse'],
  sw_unregister: ['browse'],
  sw_caches: ['browse'],
  inspect_listeners: ['browse'],

  // browse ∩ reverse (19)
  console_eval: ['browse', 'reverse'],
  console_get_props: ['browse', 'reverse'],
  cookie_list: ['browse', 'reverse'],
  cookie_set: ['browse', 'reverse'],
  cookie_delete: ['browse', 'reverse'],
  list_macros: ['browse', 'reverse'],
  get_macro: ['browse', 'reverse'],
  create_macro: ['browse', 'reverse'],
  update_macro: ['browse', 'reverse'],
  remove_macro_steps: ['browse', 'reverse'],
  run_macro: ['browse', 'reverse'],
  delete_macro: ['browse', 'reverse'],
  cli_arm: ['browse', 'reverse'],
  cli_compile: ['browse', 'reverse'],
  cli_list: ['browse', 'reverse'],
  cli_describe: ['browse', 'reverse'],
  cli_run: ['browse', 'reverse'],
  cli_writes: ['browse', 'reverse'],
  cli_export: ['browse', 'reverse'],

  // reverse, not browse (63). The four shared with probe are listed next.
  get_request_initiator: ['reverse'],
  finding_export: ['reverse'],
  finding_remove: ['reverse'],
  list_scripts: ['reverse'],
  grep_script: ['reverse'],
  grep_scripts: ['reverse'],
  extract_context: ['reverse'],
  detect_bundler: ['reverse'],
  deobfuscate_script: ['reverse'],
  resolve_source: ['reverse'],
  list_sources: ['reverse'],
  get_original_source: ['reverse'],
  deob_detect: ['reverse'],
  deob_auto: ['reverse'],
  list_wasm: ['reverse'],
  wasm_decompile: ['reverse'],
  wasm_info: ['reverse'],
  grep_wasm: ['reverse'],
  wasm_xref: ['reverse'],
  crypto_chain: ['reverse'],
  hash_iter: ['reverse'],
  hmac_compute: ['reverse'],
  magic_hash_lookup: ['reverse'],
  crypto_trace_start: ['reverse'],
  crypto_trace_list: ['reverse'],
  crypto_trace_stop: ['reverse'],
  exec_trace: ['reverse'],
  inject_add: ['reverse'],
  inject_list: ['reverse'],
  inject_remove: ['reverse'],
  inject_toggle: ['reverse'],
  inject_run_now: ['reverse'],
  intercept_add: ['reverse'],
  intercept_list: ['reverse'],
  intercept_remove: ['reverse'],
  intercept_pending: ['reverse'],
  intercept_continue: ['reverse'],
  intercept_fulfill: ['reverse'],
  intercept_fail: ['reverse'],
  override_add: ['reverse'],
  override_list: ['reverse'],
  override_remove: ['reverse'],
  header_preset_list: ['reverse'],
  header_preset_save: ['reverse'],
  header_preset_apply: ['reverse'],
  header_preset_disable: ['reverse'],
  list_websockets: ['reverse'],
  get_ws_frames: ['reverse'],
  ws_capture_enable: ['reverse'],
  ws_capture_list: ['reverse'],
  ws_send: ['reverse'],
  fetch_body: ['reverse'],
  decode_token: ['reverse'],
  protobuf_decode: ['reverse'],
  graphql_introspect: ['reverse'],
  request_diff: ['reverse'],
  find_api_base: ['reverse'],
  auth_dump: ['reverse'],
  export_python_client: ['reverse'],
  export_client: ['reverse'],
  har_export: ['reverse'],
  import_har: ['reverse'],
  detect_antibot_vm: ['reverse'],

  // reverse ∩ probe (4)
  replay_request: ['reverse', 'probe'],
  repeater_send: ['reverse', 'probe'],
  scan_secrets: ['reverse', 'probe'],
  security_inspect: ['reverse', 'probe'],

  // probe only (6) — ALWAYS_RISKY_TOOLS
  intruder_run: ['probe'],
  burst_send: ['probe'],
  payload_probe: ['probe'],
  crlf_test: ['probe'],
  path_probe: ['probe'],
  lfi_probe: ['probe'],

  // hidden (9) — external catalog only
  bp_add: ['hidden'],
  bp_list: ['hidden'],
  bp_remove: ['hidden'],
  bp_status: ['hidden'],
  bp_resume: ['hidden'],
  bp_step_over: ['hidden'],
  bp_step_into: ['hidden'],
  bp_step_out: ['hidden'],
  bp_eval_in_frame: ['hidden']
}

export const PROFILE_TOOL_BUDGET = 120

export function isToolProfile(value: unknown): value is ToolProfile {
  return value === 'browse' || value === 'reverse' || value === 'probe'
}

function isHiddenBucket(bucket: readonly ToolBucket[] | 'core'): boolean {
  return bucket !== 'core' && bucket.includes('hidden')
}

export function isAdvertised(name: string, profile: ToolProfile): boolean {
  const bucket = TOOL_BUCKETS[name]
  if (bucket == null || isHiddenBucket(bucket)) return false
  if (bucket === 'core') return true
  return bucket.includes(profile)
}

/**
 * Names advertised for `profile`. `extras` are names this session already
 * passed to mcp.callTool. Refusals must not be included by the caller.
 * Hidden and unknown extras are dropped.
 */
export function advertisedNames(profile: ToolProfile, extras?: Iterable<string>): string[] {
  const names: string[] = []
  const seen = new Set<string>()
  for (const name of Object.keys(TOOL_BUCKETS)) {
    if (!isAdvertised(name, profile)) continue
    seen.add(name)
    names.push(name)
  }
  if (extras) {
    for (const name of extras) {
      if (seen.has(name)) continue
      const bucket = TOOL_BUCKETS[name]
      if (bucket == null || isHiddenBucket(bucket)) continue
      seen.add(name)
      names.push(name)
    }
  }
  return names
}

export function assertProfileBudget(profile: ToolProfile): void {
  const n = advertisedNames(profile).length
  if (n > PROFILE_TOOL_BUDGET) {
    throw new Error(
      `tool profile ${profile} advertises ${n} tools, budget is ${PROFILE_TOOL_BUDGET}`
    )
  }
}

/** Parse a tool-profile.json body. Does not write. Invalid input returns null. */
export function readToolProfile(json: string): ToolProfile | null {
  let value: unknown
  try {
    value = JSON.parse(json)
  } catch {
    return null
  }
  if (!value || typeof value !== 'object') return null
  const row = value as Record<string, unknown>
  if (row.version !== 1 || !isToolProfile(row.profile)) return null
  return row.profile
}

export type CatalogKind = 'full' | 'follow' | 'direct'

/** Header value must be exactly `follow` or `direct`. Anything else is the full catalog. */
export function catalogFromHeader(value: string | string[] | undefined): CatalogKind {
  const raw = Array.isArray(value) ? value[0] : value
  if (raw === 'follow' || raw === 'direct') return raw
  return 'full'
}

export const TOOL_PROFILE_SET_DESCRIPTION = `Switch the in-app tool profile, or omit profile to read it.
browse (default): drive the page, read the DOM, and use the site CLI (cli_compile / cli_run / cli_export).
reverse: scripts, source maps, deobfuscation, wasm, crypto_trace, intercept/replay, codegen (export_python_client / export_client), site CLI, websockets.
probe: intruder_run, burst_send, payload_probe, crlf_test, path_probe, lfi_probe. You cannot select probe; ask the user to set Tools → Probe.
bp_* breakpoints are not available in any profile.`

export function toolProfileReadText(profile: ToolProfile, advertised: number): string {
  return `profile=${profile} advertised=${advertised}. Switch with profile "browse" or "reverse". You cannot select probe; the user sets Tools → Probe. bp_* is not in any profile.`
}

export function toolProfileSwitchedText(profile: ToolProfile, advertised: number): string {
  return `profile=${profile} advertised=${advertised}. If grep_scripts is not in your tool list yet, ask the user to send one more message.`
}

export const TOOL_PROFILE_PROBE_REJECTED =
  'probe is user-selected. Ask the user to set Tools → Probe. Profile was not changed.'

export type { ToolProfileId } from '../../shared/tool-profile-line'
export { toolProfileTurnLine } from '../../shared/tool-profile-line'

/** Live RegisteredTool handle. `enabled` is the bit applyFollowCatalog diffs. */
export interface ToolToggle {
  enabled: boolean
  enable(): void
  disable(): void
}

/**
 * Only follow sessions disable tools. full stays complete for external clients.
 * direct stays complete because the provider filters each turn instead.
 */
export function catalogDisablesTools(catalog: CatalogKind): boolean {
  return catalog === 'follow'
}

/**
 * Enable the tools `profile` advertises and disable the rest.
 * While toggling, `sendToolListChanged` is a no-op. If any bit flipped and the
 * server is connected, the original notification runs once. No bit change, or
 * a server that is not connected yet, sends nothing.
 */
export function applyFollowCatalog(
  mcp: { isConnected(): boolean; sendToolListChanged: () => void },
  handles: ReadonlyMap<string, ToolToggle>,
  profile: ToolProfile
): void {
  const original = mcp.sendToolListChanged.bind(mcp)
  mcp.sendToolListChanged = () => {}
  let changed = false
  try {
    for (const [name, handle] of handles) {
      const want = isAdvertised(name, profile)
      if (want === handle.enabled) continue
      if (want) handle.enable()
      else handle.disable()
      changed = true
    }
  } finally {
    mcp.sendToolListChanged = original
  }
  if (changed && mcp.isConnected()) original()
}
