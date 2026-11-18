export const available = true

export async function resolveBinary() {
  return process.env.DEVCODE_PTY_BIN || "devcode-pty"
}
