// English UI strings — the source of truth. `ko.ts` mirrors these keys.
// Placeholders use {name} and are filled by the t() helper.
export const en = {
  // Agent pane
  'agent.chat': 'Chat (ACP)',
  'agent.terminal': 'Terminal (CLI)',

  // Terminal (CLI) onboarding
  'cli.title': 'Rever in your terminal',
  'cli.desc': "Run Claude Code in a real terminal, already wired to Rever's browser and traffic tools.",
  'cli.start': 'Start Claude Code →',
  'cli.skillPrompt': 'Want /rever in any Claude Code session? Run this once:',
  'cli.copy': 'Copy',
  'cli.copied': 'Copied ✓',
  'cli.installed': '✓ /rever skill already installed',
  'cli.requires': 'Requires the claude CLI (your own subscription). macOS/Linux.',
  'cli.guide': 'Guide',
  'cli.guideTitle': 'Show CLI mode guide',
  'cli.exited': 'Agent exited (code {code}).',
  'cli.restart': 'Restart',

  // Tabs & profiles
  'tab.new': 'New tab',
  'tab.close': 'Close tab',
  'tab.newInProfile': 'New tab in profile',
  'profile.openIn': 'Open in profile',
  'profile.newName': 'New profile name',
  'profile.persistent': '+ Persistent',
  'profile.incognito': '+ Incognito',
  'profile.persistentHint': 'Create a persistent profile (survives restart)',
  'profile.incognitoHint': 'Create an incognito profile (in-memory, cleared on quit)',
  'profile.importFrom': 'Import profile from browser',
  'profile.delete': 'Delete profile',
  'profile.incognitoTag': 'incognito',

  // Toolbar
  'toolbar.viewport.desktop': 'Desktop',
  'toolbar.viewport.mobile': 'Mobile',
  'toolbar.embedded': 'Embedded',
  'toolbar.external': 'External',
  'toolbar.viewSource': 'View page source in a new tab (view-source:)',
  'toolbar.pick': 'Pick an element (copies selector + ref)',
  'toolbar.grab': 'Grab an element (screenshot + context → chat & markup)',
  'toolbar.addressPlaceholder': 'https://...',
  'toolbar.theme': 'Theme: {mode} — click to cycle System → Light → Dark',
  'toolbar.theme.dark': 'Dark',
  'toolbar.theme.light': 'White',
  'toolbar.theme.system': 'Com',
  'toolbar.openChat': 'Open chat',
  'toolbar.collapseChat': 'Collapse chat',

  // Markup editor
  'markup.rect': 'Rectangle',
  'markup.arrow': 'Arrow',
  'markup.pen': 'Pen',
  'markup.undo': 'Undo',
  'markup.clear': 'Clear',
  'markup.copy': 'Copy',
  'markup.copied': 'Copied ✓',
  'markup.save': 'Save PNG',
  'markup.close': 'Close',

  // Cookies panel — browser import
  'cookies.importFrom': 'Import cookies from',
  'cookies.importBrowserTitle': 'Browser to import from',
  'cookies.importProfileTitle': 'Profile to import from',
  'cookies.domainsPlaceholder': 'domains, comma-separated (blank = all)',
  'cookies.domainsTitle': 'Only import cookies whose host contains one of these substrings',
  'cookies.import': 'Import',
  'cookies.importing': 'Importing…',

  // Language switcher
  'lang.label': 'Language',

  // Common
  'common.cancel': 'Cancel',
  'common.edit': 'Edit',
  'common.delete': 'Delete',
  'common.remove': 'Remove',
  'common.stop': 'Stop',
  'common.clear': 'Clear',
  'common.moveUp': 'Move up',
  'common.moveDown': 'Move down',

  // Panels (empty states)
  'panel.noConsole': 'No console output',
  'toast.downloaded': 'Downloaded {name}',
  'toast.downloadFailed': 'Download {state}: {name}',
  'toast.showInFolder': 'Show in folder',
  'toast.dialogAnswered': 'Page {type} answered {answer} for the agent: {message}',
  'settings.dialogAuto': 'Answer page dialogs while the agent works',
  'settings.dialogAutoHint': 'Otherwise alert / confirm show up for you to answer. Contents are logged.',
  'settings.downloadAsk': 'Ask where to save downloads',
  'settings.downloadAskHint': 'Off: saved to Downloads/Rever without a dialog, so the agent never gets stuck.',
  'panel.consoleInput': 'Run JavaScript in this tab (Enter to run, Shift+Enter for a new line)',
  'panel.noExceptions': 'No exceptions',
  'panel.noWs': 'No WS connections',
  'panel.noFrames': 'No frames',
  'panel.selectConnection': 'Select a connection',
  'history.filter': 'Filter by URL or title',

  // Traffic
  'traffic.clear': 'Clear captured traffic',
  'traffic.filter': 'Filter URL or method…',
  'traffic.method': 'Method',
  'traffic.status': 'Status',
  'traffic.type': 'Type',
  'traffic.url': 'URL',
  'traffic.size': 'Size',
  'traffic.sendToRepeater': 'Send to Repeater',

  // Traffic detail drawer
  'drawer.request': 'Request',
  'drawer.response': 'Response',
  'drawer.queryParams': 'Query params',
  'drawer.responseBody': 'Response body',

  // JSON body viewer
  'json.pretty': 'Pretty',
  'json.raw': 'Raw',
  'json.copy': 'Copy',
  'json.copied': 'Copied',
  'json.items': '{n} items',
  'json.keys': '{n} keys',
  'json.more': '… {n} more',

  // Repeater
  'repeater.empty': 'In Traffic, click the "↻R" (Send to Repeater) button on a request row to load it here.',
  'repeater.removeHeader': 'Remove header',

  // Chat
  'chat.input': 'input',
  'chat.output': 'output',
  'chat.switchModel': 'Switch model for this session',
  'chat.turnFailed': 'Turn failed',
  'chat.refusedTitle': 'Request blocked by safety policy',
  'chat.refused':
    'This request was flagged by the model provider’s safety policy, so the agent stopped and did not run it. Rephrasing with clear context about your authorized task may help; otherwise this request cannot proceed.',
  'chat.safeguardTitle': 'Safety fallback occurred',
  'chat.safeguard':
    'The model provider’s safeguard flagged this request once and the session continued on a fallback model. The answer may differ from usual — if it looks incomplete, rephrase with clear context about your authorized task.',
  'chat.modelSwitchFailed': 'This agent did not accept the model switch',
  'chat.newConversation': 'Start a new conversation (kills the current agent session)',
  'chat.placeholder': 'Type a message…',
  'chat.historyTitle': 'Conversation history',
  'chat.history': 'History',
  'chat.deleteConversation': 'Delete conversation',
  'chat.chooseAgent': 'Choose AI agent',
  'chat.chooseAgentShort': 'Choose agent',

  // Macro editor
  'macro.goalPlaceholder': 'Login then dump the profile API',
  'macro.steps': 'Steps',
  'macro.removeStep': 'Remove step',
  'macro.waitForPlaceholder': 'wait for selector (optional)',
  'macro.waitForTitle': 'Poll until this CSS selector matches, then run the step. Fails the step after 10s.',
  'macro.delayPlaceholder': 'delay ms',
  'macro.delayTitle': 'Extra pause before the step, applied after waitFor',
  'macro.saveAsPlaceholder': 'save as',
  'macro.saveAsTitle': "Store this step's result under this name; use it later as {{name}}",

  // Pipeline editor
  'pipeline.ifLast': 'if last',
  'pipeline.valuePlaceholder': 'value / regex',
  'pipeline.goalPlaceholder': 'Probe endpoint, branch on 200 vs 403',

  // Template editor
  'template.goalPlaceholder': "Analyze this site's auth flow",
  'template.bodyPlaceholder': 'Text inserted into the agent chat input when you click Use.',

  // First-run agent onboarding
  'onboard.title': 'Connect your AI provider',
  'onboard.desc': 'Rever drives the browser through an AI agent. Use a subscription you already have, or paste an API key.',
  'onboard.scanning': 'Checking your machine…',
  'onboard.ready': '{n} ready',
  'onboard.use': 'Use',
  'onboard.inUse': 'In use',
  'model.search': 'Search models…',
  'model.noMatch': 'No models match',
  'onboard.skip': 'Skip for now',
  'onboard.recheck': 'Re-check',
  'onboard.save': 'Save',
  'onboard.keyPlaceholder': 'Paste API key…',
  'onboard.installWith': 'Install with',
  'onboard.signInWith': 'Sign in with',
  'onboard.copy': 'Copy',
  'onboard.copied': 'Copied \u2713',
  'onboard.plan': '{plan} plan',
  'onboard.status.ready': 'Ready',
  'onboard.status.auto': 'Connected automatically',
  'onboard.status.not-installed': 'Not installed',
  'onboard.status.needs-key': 'Needs API key',
  'onboard.status.needs-login': 'Sign-in required',
  'onboard.status.auth-failed': 'Key rejected',
  'onboard.status.failed': 'Failed to start',

  // Address-bar toolbar (App)
  'toolbar.back': 'Back',
  'toolbar.forward': 'Forward',
  'toolbar.reload': 'Reload',
  'toolbar.hardReload': 'Hard Reload (clear cache)',
  'toolbar.go': 'Go',
  'toolbar.findInPage': 'Find in page',

  // Proxy button
  'proxy.label': 'Proxy',
  'proxy.labelActive': 'Proxy: {host}:{port}',
  'proxy.setTitle': 'Set a proxy for the browser',
  'proxy.editTitle': 'Proxy: {scheme}://{host}:{port} — click to edit',
  'proxy.tabProxy': 'Tab proxy',
  'proxy.scheme': 'Scheme',
  'proxy.host': 'Host',
  'proxy.port': 'Port',
  'proxy.user': 'User',
  'proxy.optional': '(optional)',
  'proxy.password': 'Password',
  'proxy.disable': 'Disable',
  'proxy.apply': 'Apply',
  'proxy.openTab': 'Open proxy tab',
  'proxy.errHost': 'Host is required',
  'proxy.errPort': 'Port must be 1–65535',
  'proxy.errApply': 'Failed to apply proxy',
  'proxy.noteEdit':
    'This tab runs in its own session (separate cookies, incognito-style). Changes apply to this tab only and reload the page.',
  'proxy.noteNew':
    'Opens the current page in a new tab with its own cookies and storage (like incognito), routed through this proxy. Other tabs are unaffected.',

  // Bot-detection probes
  'botcheck.trigger': 'Bot check',
  'botcheck.triggerTitle': 'Open bot detection test sites',
  'botcheck.menuTitle': 'Bot detection probes',
  'botcheck.sannysoft.desc': 'Baseline stealth (webdriver / plugins / UA)',
  'botcheck.creepjs.desc': 'Full-stack fingerprint (canvas / audio / font / WebGL)',
  'botcheck.browserleaks.desc': 'Individual leaks (Canvas / WebGL / WebRTC)',
  'botcheck.amiunique.desc': 'Uniqueness score',
  'botcheck.pixelscan.desc': 'Consistency cross-check (UA / OS / WebGL)',

  // Cookies panel section tabs
  'cookies.tab.cookies': 'Cookies',
  'cookies.tab.local': 'localStorage',
  'cookies.tab.session': 'sessionStorage',
  'cookies.refresh': 'Refresh',

  // Egress IP badge
  'ip.hiddenTitle':
    'Egress IP is hidden — click "Show" to reveal, or click here to refresh in the background',
  'ip.errorTitle': 'Failed to fetch egress IP: {error} — click to retry',
  'ip.title': 'Egress IP of the active tab (through its proxy, if set) — click to refresh',
  'ip.masked': 'IP: •••••••',
  'ip.none': 'IP: —',
  'ip.show': 'Show',
  'ip.hide': 'Hide',
  'ip.showTitle': 'Show the egress IP',
  'ip.hideTitle': 'Hide the egress IP (e.g. before recording)',
  'ip.unknownError': 'Unknown error',

  // Copy confirmation
  'copy.copied': 'Copied!',
  'copy.toClipboard': '✓ Copied to clipboard',

  // Bookmark bar
  'bookmark.remove': 'Remove bookmark',

  // Agent permission prompt
  'perm.default': 'The agent requested permission',
  'perm.required': 'Permission required',
  'perm.moreQueued': '· {n} more queued',
  'perm.keys': 'Enter = allow · Esc = reject',
  'perm.dialogLabel': 'Agent permission request',
  'perm.riskyTool': 'Risky action · {tool}',
  'perm.target': 'Target',
  'perm.allowSession': 'Allow all {tool} calls this session',

  'tools.profile.label': 'Tools',
  'tools.profile.browse': 'Browse',
  'tools.profile.reverse': 'Reverse',
  'tools.profile.probe': 'Probe',
  'tools.profile.tip.browse': 'Page and DOM tools. API reversing tools are hidden.',
  'tools.profile.tip.reverse': 'Scripts, crypto, replay, and client export.',
  'tools.profile.tip.probe':
    'Active probes. Hidden until you select Probe. ACP chat still prompts before each risky call. API models get no new confirm; they run the call the way they do today.',

  'workflow.cli.name': 'Name',
  'workflow.cli.requests': 'API calls to compile',
  'workflow.cli.empty': 'No XHR or Fetch calls yet. Use the page, then compile the ones you pick.',
  'workflow.cli.compile': 'Compile',
  'workflow.cli.compiling': 'Compiling…',
  'workflow.cli.commands': 'Commands',
  'workflow.cli.noCommands': 'Pick calls and compile.',
  'workflow.cli.noMatch': 'None of the selected calls became a command.',
  'workflow.cli.writes': 'Allow writes',
  'workflow.cli.writesHint': 'Off by default. Turn this on only when you mean to send a change.',
  'workflow.cli.sessionHint': 'Cookies stay in this tab. The export uses placeholders only.',
  'workflow.cli.export': 'Export',
  'workflow.cli.run': 'Run',
  'workflow.cli.running': 'Running…',
  'workflow.cli.runFailed': 'Run failed',
  'workflow.cli.extract': 'Returns up to {limit} items: {selector}',
  'workflow.cli.matched': 'Matched {count}',
  'workflow.cli.noMatchExtract': 'The selector matched nothing.',
  'workflow.cli.pageCapped': 'The page was longer than the read cap.',
  'workflow.cli.bodyCut':
    'This is only the start of the page. It is not the saved command. Add an extract selector to get the fields.',
  'workflow.cli.exportHeading': 'Export',
  'workflow.cli.resultHeading': 'Run result',
  'workflow.cli.callExample': 'Call example',
  'workflow.cli.callExampleHint':
    'This is the cli_run call for this command. Export is curl with placeholders. Cookie values are not included.'
}

export type TKey = keyof typeof en
