/* eslint-disable */
// Native addon loader for markit. Exports the binding object or null.

const { existsSync, readFileSync } = require('fs');
const { join } = require('path');

const { platform, arch } = process;

let nativeBinding = null;
let loadError = null;

function isMusl() {
  if (!process.report || typeof process.report.getReport !== 'function') {
    try {
      const lddPath = require('child_process').execSync('which ldd').toString().trim();
      return readFileSync(lddPath, 'utf8').includes('musl');
    } catch {
      return true;
    }
  } else {
    const { glibcVersionRuntime } = process.report.getReport().header;
    return !glibcVersionRuntime;
  }
}

function tryLoad(localFile, packageName) {
  const localPath = join(__dirname, localFile);
  if (existsSync(localPath)) {
    return require(localPath);
  }
  // Also check rust/ build output for development
  const devPath = join(__dirname, 'rust', localFile);
  if (existsSync(devPath)) {
    return require(devPath);
  }
  return require(packageName);
}

try {
  switch (platform) {
    case 'darwin':
      switch (arch) {
        case 'x64':
          nativeBinding = tryLoad('markit.darwin-x64.node', 'markit-ai-darwin-x64');
          break;
        case 'arm64':
          nativeBinding = tryLoad('markit.darwin-arm64.node', 'markit-ai-darwin-arm64');
          break;
        default:
          throw new Error(`Unsupported architecture on macOS: ${arch}`);
      }
      break;
    case 'linux':
      switch (arch) {
        case 'x64':
          if (isMusl()) {
            nativeBinding = tryLoad('markit.linux-x64-musl.node', 'markit-ai-linux-x64-musl');
          } else {
            nativeBinding = tryLoad('markit.linux-x64-gnu.node', 'markit-ai-linux-x64-gnu');
          }
          break;
        case 'arm64':
          if (isMusl()) {
            nativeBinding = tryLoad('markit.linux-arm64-musl.node', 'markit-ai-linux-arm64-musl');
          } else {
            nativeBinding = tryLoad('markit.linux-arm64-gnu.node', 'markit-ai-linux-arm64-gnu');
          }
          break;
        default:
          throw new Error(`Unsupported architecture on Linux: ${arch}`);
      }
      break;
    default:
      // Unsupported platform — fall through to null
      break;
  }
} catch (e) {
  loadError = e;
  nativeBinding = null;
}

if (loadError && process.env.MARKIT_DEBUG) {
  console.error('[markit] native addon unavailable, using TS fallback:', loadError.message);
}

module.exports = nativeBinding;
