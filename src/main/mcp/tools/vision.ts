import { z } from 'zod'
import Anthropic from '@anthropic-ai/sdk'
import OpenAI from 'openai'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { emitAiAction } from '../../ai-events'
import { getActiveTarget } from '../../chrome-cdp'
import { getApiKey } from '../../settings'
import { ok, err, errorMessage } from '../utils'

// 현재 페이지를 스크린샷으로 찍어 비전 모델에게 "판단"을 맡기는 도구.
// 접근성 트리(browser_snapshot)로는 안 잡히는 시각적 상태 — 레이아웃, 팝업/모달,
// 캡차, 광고 오버레이, "검색이 실제로 됐는지", "결과가 화면에 떴는지" 등 —
// 을 사람 눈처럼 확인할 때 쓴다. Anthropic 키가 있으면 Claude, 없으면 OpenAI로.

const ANTHROPIC_VISION_MODEL = 'claude-sonnet-5'
const OPENAI_VISION_MODEL = 'gpt-4o'

const SYSTEM_PROMPT =
  'You are a vision judge for a browser-automation agent. You are shown a screenshot of ' +
  'the current web page. Answer the question precisely and concisely, describing only what ' +
  'is actually visible. If asked where an element is or how to interact with it, describe its ' +
  'visible location and, when you can reasonably infer it, suggest a CSS selector. If asked a ' +
  'yes/no judgement (e.g. did the search run, is a modal blocking the page), answer with a clear ' +
  'yes/no plus the visual evidence. Never invent content that is not on screen.'

async function judgeWithAnthropic(
  apiKey: string,
  model: string,
  question: string,
  pngBase64: string
): Promise<string> {
  const anthropic = new Anthropic({ apiKey })
  const msg = await anthropic.messages.create({
    model,
    max_tokens: 1024,
    system: SYSTEM_PROMPT,
    messages: [
      {
        role: 'user',
        content: [
          {
            type: 'image',
            source: { type: 'base64', media_type: 'image/png', data: pngBase64 }
          },
          { type: 'text', text: question }
        ]
      }
    ]
  })
  return msg.content
    .filter((b): b is Anthropic.TextBlock => b.type === 'text')
    .map((b) => b.text)
    .join('\n')
    .trim()
}

async function judgeWithOpenAI(
  apiKey: string,
  model: string,
  question: string,
  pngBase64: string
): Promise<string> {
  const openai = new OpenAI({ apiKey })
  const resp = await openai.chat.completions.create({
    model,
    max_tokens: 1024,
    messages: [
      { role: 'system', content: SYSTEM_PROMPT },
      {
        role: 'user',
        content: [
          { type: 'text', text: question },
          {
            type: 'image_url',
            image_url: { url: `data:image/png;base64,${pngBase64}` }
          }
        ]
      }
    ]
  })
  return (resp.choices[0]?.message?.content ?? '').trim()
}

export function registerVisionTools(mcp: McpServer) {
  mcp.registerTool(
    'vision_judge',
    {
      description:
        'Capture a screenshot of the current page and ask a vision model to judge it. Use for things the accessibility snapshot cannot show: whether an action visually succeeded (did the search run? did results render?), whether a modal/captcha/ad overlay is blocking the page, reading text baked into images/canvas, or locating an element visually. Returns the model\'s answer as text. Uses Claude if an Anthropic key is set, otherwise OpenAI.',
      inputSchema: {
        question: z
          .string()
          .describe(
            'What to decide from the screenshot, e.g. "Did the search for 오늘 날씨 actually run and show results?" or "Is anything covering the search box?"'
          ),
        model: z
          .string()
          .optional()
          .describe(
            'Optional vision model override for the selected provider (default: claude-sonnet-5 for Anthropic, gpt-4o for OpenAI).'
          )
      }
    },
    async ({ question, model }) => {
      const target = getActiveTarget()
      if (!target) return err('no active browser target — open a page first')

      const anthropicKey = getApiKey('anthropic')
      const openaiKey = getApiKey('openai')
      if (!anthropicKey && !openaiKey) {
        return err(
          'no vision API key — add an Anthropic or OpenAI API key in settings to use vision_judge'
        )
      }

      try {
        emitAiAction({ kind: 'screenshot', label: 'AI vision', detail: question.slice(0, 80) })
        const img = await target.wc.capturePage()
        const pngBase64 = img.toPNG().toString('base64')

        let provider: string
        let usedModel: string
        let answer: string
        if (anthropicKey) {
          provider = 'anthropic'
          usedModel = model ?? ANTHROPIC_VISION_MODEL
          answer = await judgeWithAnthropic(anthropicKey, usedModel, question, pngBase64)
        } else {
          provider = 'openai'
          usedModel = model ?? OPENAI_VISION_MODEL
          answer = await judgeWithOpenAI(openaiKey!, usedModel, question, pngBase64)
        }

        return ok(`(vision: ${provider}/${usedModel})\n\n${answer || '(empty response)'}`)
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )
}
