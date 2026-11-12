export interface VakaPluginOptions {
  /** Vaka server URL. Default: VAKA_URL. */
  url?: string;
  /** API token with write scope (vt_...). Default: VAKA_TOKEN. */
  token?: string;
  /** Project slug or id. Default: VAKA_PROJECT. */
  project?: string;
  /** Release; must equal the `release` passed to the SDK. Default: VAKA_RELEASE. */
  release?: string;
  /** URL path the files are served from. Default: Vite `base` or webpack `output.publicPath`, else "/". */
  urlPrefix?: string;
  /** Delete .map files from the output after uploading, so they are not deployed. */
  deleteAfterUpload?: boolean;
  /** Fail the build when the upload fails or is not configured. Default: true when CI is set. */
  failOnError?: boolean;
  /** Log what would be uploaded without uploading. */
  dryRun?: boolean;
  /** Define `__VAKA_RELEASE__` in app code with the release. Default true. */
  defineRelease?: boolean;
  /** webpack: also upload in development mode. */
  uploadInDevelopment?: boolean;
  silent?: boolean;
}

export interface UploadResult {
  uploaded: number;
  skipped: boolean;
  errors: string[];
}

/** Vite/Rollup plugin: uploads source maps after the bundle is written. */
export function vakaVite(options?: VakaPluginOptions): any;
export const vakaRollup: typeof vakaVite;
/** webpack 5 plugin: uploads source maps after emit. */
export class VakaWebpackPlugin {
  constructor(options?: VakaPluginOptions);
  apply(compiler: any): void;
}
/** Upload every *.map file under a directory. */
export function uploadSourcemaps(dir: string, options?: VakaPluginOptions): Promise<UploadResult>;
export function artifactName(urlPrefix: string | undefined, relativeMapPath: string): string;
export default vakaVite;
