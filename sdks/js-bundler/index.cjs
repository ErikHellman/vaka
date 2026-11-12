'use strict';
// Upload source maps to Vaka after a Vite, Rollup or webpack build. No dependencies; Node 18+.
const fs = require('node:fs');
const path = require('node:path');

function resolveOptions(o = {}) {
  const env = process.env;
  return {
    url: (o.url || env.VAKA_URL || '').replace(/\/+$/, ''),
    token: o.token || env.VAKA_TOKEN || '',
    project: o.project || env.VAKA_PROJECT || '',
    release: o.release || env.VAKA_RELEASE || '',
    urlPrefix: o.urlPrefix,
    deleteAfterUpload: !!o.deleteAfterUpload,
    failOnError: o.failOnError ?? !!env.CI,
    dryRun: !!o.dryRun,
    defineRelease: o.defineRelease ?? true,
    log: o.silent ? () => {} : (msg) => console.log(`[vaka] ${msg}`),
    warn: (msg) => console.warn(`[vaka] ${msg}`),
  };
}

function findMaps(dir) {
  const out = [];
  const walk = (d) => {
    let entries;
    try { entries = fs.readdirSync(d, { withFileTypes: true }); } catch { return; }
    for (const e of entries) {
      const p = path.join(d, e.name);
      if (e.isDirectory()) walk(p);
      else if (e.isFile() && e.name.endsWith('.map')) out.push(p);
    }
  };
  walk(dir);
  return out.sort();
}

/** The artifact name the server matches against the minified file's URL path. */
function artifactName(prefix, rel) {
  const p = String(prefix || '/').replace(/^~/, '').replace(/^https?:\/\/[^/]+/, '').replace(/\/+$/, '');
  const minified = rel.split(path.sep).join('/').replace(/\.map$/, '');
  return `${p}/${minified}`;
}

/**
 * Upload every *.map under `dir`. Returns {uploaded, skipped, errors}.
 * Missing configuration skips the upload with a warning (so local builds keep working)
 * unless failOnError is set.
 */
async function uploadSourcemaps(dir, options = {}) {
  const o = options.__resolved ? options : resolveOptions(options);
  const missing = ['url', 'token', 'project', 'release'].filter((k) => !o[k]);
  if (missing.length) {
    const msg = `source map upload skipped: set ${missing.map((k) => `VAKA_${k.toUpperCase()}`).join(', ')} (or plugin options)`;
    if (o.failOnError) throw new Error(msg);
    o.warn(msg);
    return { uploaded: 0, skipped: true, errors: [] };
  }
  const maps = findMaps(dir);
  if (!maps.length) {
    const msg = `no .map files in ${dir}; enable source maps in the bundler (e.g. build.sourcemap: 'hidden')`;
    if (o.failOnError) throw new Error(msg);
    o.warn(msg);
    return { uploaded: 0, skipped: false, errors: [] };
  }
  const endpoint = `${o.url}/api/v1/projects/${encodeURIComponent(o.project)}/artifacts`;
  const errors = [];
  let uploaded = 0;
  for (const file of maps) {
    const name = artifactName(o.urlPrefix, path.relative(dir, file));
    if (o.dryRun) {
      o.log(`would upload ${file} as ${name}`);
      continue;
    }
    try {
      const form = new FormData();
      form.append('kind', 'sourcemap');
      form.append('release', o.release);
      form.append('name', name);
      form.append('file', new Blob([fs.readFileSync(file)]), path.basename(file));
      const res = await fetch(endpoint, { method: 'POST', headers: { authorization: `Bearer ${o.token}` }, body: form });
      if (!res.ok) {
        const text = await res.text().catch(() => '');
        throw new Error(`${res.status} ${text.slice(0, 200)}`);
      }
      uploaded++;
      if (o.deleteAfterUpload) fs.rmSync(file);
    } catch (e) {
      errors.push(`${name}: ${e.message}`);
    }
  }
  if (errors.length) {
    const msg = `${errors.length} source map upload(s) failed:\n  ${errors.join('\n  ')}`;
    if (o.failOnError) throw new Error(msg);
    o.warn(msg);
  }
  o.log(`${uploaded} source map(s) uploaded for release ${o.release}${o.deleteAfterUpload ? ' (deleted from the output)' : ''}`);
  return { uploaded, skipped: false, errors };
}

/** Vite and Rollup plugin. */
function vakaVite(options = {}) {
  const o = { ...resolveOptions(options), __resolved: true };
  let outDir = 'dist';
  let base;
  let isBuild = true;
  return {
    name: 'vaka-sourcemaps',
    apply: 'build',
    config(config) {
      // Vite: make the release available to app code as __VAKA_RELEASE__.
      if (o.defineRelease && o.release) return { define: { __VAKA_RELEASE__: JSON.stringify(o.release) } };
    },
    configResolved(config) {
      outDir = path.resolve(config.root || process.cwd(), config.build?.outDir || 'dist');
      base = config.base;
      isBuild = config.command !== 'serve';
    },
    async writeBundle(outputOptions) {
      if (!isBuild) return;
      const dir = outputOptions.dir ? path.resolve(outputOptions.dir) : outputOptions.file ? path.dirname(path.resolve(outputOptions.file)) : outDir;
      const prefix = o.urlPrefix ?? (base && base !== './' ? base : '/');
      // writeBundle runs once per output; upload each directory once.
      this.__vakaDone = this.__vakaDone || new Set();
      if (this.__vakaDone.has(dir)) return;
      this.__vakaDone.add(dir);
      await uploadSourcemaps(dir, { ...o, urlPrefix: prefix });
    },
  };
}

/** webpack 5 plugin. */
class VakaWebpackPlugin {
  constructor(options = {}) {
    this.o = { ...resolveOptions(options), __resolved: true };
  }
  apply(compiler) {
    const o = this.o;
    if (o.defineRelease && o.release && compiler.webpack?.DefinePlugin) {
      new compiler.webpack.DefinePlugin({ __VAKA_RELEASE__: JSON.stringify(o.release) }).apply(compiler);
    }
    compiler.hooks.afterEmit.tapPromise('VakaWebpackPlugin', async (compilation) => {
      if (compiler.options.mode === 'development' && !o.uploadInDevelopment) return;
      const dir = compilation.outputOptions?.path || compiler.outputPath;
      let publicPath = compilation.outputOptions?.publicPath ?? compiler.options.output?.publicPath;
      if (typeof publicPath !== 'string' || publicPath === 'auto') publicPath = '/';
      await uploadSourcemaps(dir, { ...o, urlPrefix: o.urlPrefix ?? publicPath });
    });
  }
}

module.exports = { vakaVite, vakaRollup: vakaVite, VakaWebpackPlugin, uploadSourcemaps, artifactName };
