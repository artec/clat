/**
 * Demo DSH plugin (the Phase 2 `probe` equivalent): four tools exercising
 * the pure path, sampling, elicitation, and the CLAT host-service projection.
 * Definitions are hand-rolled in the compiled shape `defineTool()` produces
 * — real authors use defineTool from `@deepseek-ai/dsh-tools`; its output is
 * structurally identical.
 */

import { serveClat } from './index.js'
import type { DshContext, DshPluginLike, ToolDefinitionLike } from './types.js'

function textTool(tool: {
  name: string
  description: string
  properties: Record<string, Record<string, unknown>>
  required?: string[]
  execute: (args: Record<string, unknown>) => Promise<unknown> | unknown
}): ToolDefinitionLike {
  return {
    name: tool.name,
    description: tool.description,
    parameters: {
      type: 'object',
      properties: tool.properties,
      ...(tool.required === undefined ? {} : { required: tool.required }),
    },
    output: {
      render: (_args: unknown, value: unknown) => [{ type: 'text', text: JSON.stringify(value, null, 2) }],
    },
    execute: async (args: unknown) => tool.execute((args ?? {}) as Record<string, unknown>),
  }
}

export const demoPlugin: DshPluginLike = {
  name: 'demo',
  apply(ctx: DshContext): void {
    ctx.systemPrompt.section({
      name: 'demo:guidance',
      order: 100,
      text: 'Demo plugin is active in {{cwd}}.',
    })
    ctx.tools.register(textTool({
      name: 'echo',
      description: 'Echo text back, repeated `times` times (1-8, default 1).',
      properties: {
        text: { type: 'string', description: 'Text to echo' },
        times: { type: 'integer', description: 'Repetitions (1-8)' },
      },
      required: ['text'],
      execute: args => {
        const text = args['text']
        const rawTimes = args['times']
        const times = typeof rawTimes === 'number' ? Math.min(8, Math.max(1, Math.floor(rawTimes))) : 1
        if (typeof text !== 'string' || text === '') throw new Error('echo: `text` must be a non-empty string')
        return { text, lines: Array.from({ length: times }, () => text) }
      },
    }))

    ctx.tools.register(textTool({
      name: 'sample_roundtrip',
      description: 'Ask the host model one question through ctx.llm.stream and return the assembled answer.',
      properties: {
        prompt: { type: 'string', description: 'The question for the host model' },
      },
      required: ['prompt'],
      execute: async args => {
        const prompt = args['prompt']
        if (typeof prompt !== 'string' || prompt === '') throw new Error('sample_roundtrip: `prompt` must be a non-empty string')
        let text = ''
        for await (const chunk of ctx.llm.stream({
          messages: [{ role: 'user', content: [{ type: 'text', text: prompt }] }],
          maxTokens: 64,
        })) {
          if (chunk.type === 'text-delta') text += chunk.text
        }
        return { prompt, answer: text }
      },
    }))

    ctx.tools.register(textTool({
      name: 'ask_roundtrip',
      description: 'Ask the user three questions through ctx.userQuestions.ask (single-select, multi-select, free text) and echo the structured answer.',
      properties: {},
      execute: async () => {
        const answer = await ctx.userQuestions.ask({
          questions: [
            {
              id: 'flavor',
              question: 'Which flavor do you want?',
              options: [
                { label: 'vanilla', description: 'the classic' },
                { label: 'pistachio', description: 'the green one' },
              ],
            },
            {
              id: 'toppings',
              question: 'Which toppings?',
              multiSelect: true,
              options: [{ label: 'sprinkles' }, { label: 'fudge' }, { label: 'cherries' }],
            },
            { id: 'note', question: 'Any note for the kitchen?' },
          ],
        })
        return { answers: answer.answers }
      },
    }))

    ctx.tools.register(textTool({
      name: 'host_roundtrip',
      description: 'Exercise DSH-shaped fs, shell, sessions, and agents over the CLAT host contract.',
      properties: {
        path: { type: 'string', description: 'Project-relative text file to read' },
      },
      required: ['path'],
      execute: async args => {
        const path = args['path']
        if (typeof path !== 'string' || path === '') throw new Error('host_roundtrip: `path` must be a non-empty string')
        const context = await ctx.clat.context()
        const target = await ctx.fs.resolve(path)
        const text = await ctx.fs.readText(target)
        const shellSpec = ctx.shell.resolve({ command: 'printf dsh-shell' })
        const shell = await ctx.shell.run(shellSpec) as { stdout?: { text?: string } }
        return {
          projectRoot: context.project.root,
          text,
          stdout: shell.stdout?.text ?? '',
          sessionIds: ctx.sessions.list().map(session => session.id),
          agentIds: ctx.agents.list().map(agent => agent.id),
        }
      },
    }))
  },
}

/** Spawn-ready entry for the Rust e2e test: `node tools/demo-bin.mjs`. */
export function serveDemoPlugin(): Promise<import('./index.js').RunningAdapter> {
  return serveClat(demoPlugin, { name: 'demo', version: '0.0.0' })
}
