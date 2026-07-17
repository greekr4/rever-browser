// Workflow subsystem barrel. Each `import './<module>'` below is a side-effect
// import that registers one workflow kind. To remove a kind, delete its folder
// and its line here — the core (panel/store) needs no other changes.
import './template'

export { WorkflowPanel } from './core/WorkflowPanel'
