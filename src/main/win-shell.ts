// Windows has to launch agent CLIs through cmd.exe: npm installs them as
// `.cmd` shims, and Node's CVE-2024-27980 patch refuses to spawn `.cmd`/`.bat`
// without `shell: true` (EINVAL). But with `shell: true` Node just joins
// `[command, ...args]` with spaces and hands the line to `cmd.exe /d /s /c`,
// unquoted — so an empty argument vanishes (`--setting-sources ''` became
// `--setting-sources --permission-mode`), JSON loses its quotes, and paths with
// spaces split. Quote everything ourselves so each argument arrives intact.

// cmd.exe metacharacters (same set cross-spawn escapes). `"` is included so
// cmd never toggles its quote state mid-argument: every char is literal to cmd.
const CMD_META = /([()\][%!^"`<>&|;, *?])/g

/**
 * Quote one argument for a cmd.exe command line: first by the MSVCRT /
 * CommandLineToArgvW rules the target program parses with, then caret-escape
 * cmd's metacharacters so cmd passes it through untouched.
 */
export function quoteCmdArg(arg: string): string {
  const msvcrt = `"${arg.replace(/(\\*)"/g, '$1$1\\"').replace(/(\\*)$/, '$1$1')}"`
  return msvcrt.replace(CMD_META, '^$1')
}

/** Quote the program path itself (it may live under `C:\Program Files`). */
export function quoteCmdCommand(command: string): string {
  return `"${command.replace(/"/g, '')}"`
}

/**
 * `spawn` arguments that survive the platform's launch path: on Windows the
 * command line is pre-quoted for cmd.exe and run with `shell: true`; elsewhere
 * the args go straight to the executable with no shell.
 */
export function shellSafeSpawn(
  command: string,
  args: readonly string[],
  windows: boolean = process.platform === 'win32'
): { command: string; args: string[]; shell: boolean } {
  if (!windows) return { command, args: [...args], shell: false }
  // One pre-built command line with no args array: Node would only join them
  // with spaces anyway, and passing args with `shell: true` is deprecated (DEP0190).
  return { command: [quoteCmdCommand(command), ...args.map(quoteCmdArg)].join(' '), args: [], shell: true }
}
