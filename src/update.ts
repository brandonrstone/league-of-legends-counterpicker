import type { AppSnapshot } from "./types";

const GITHUB_REPO = "brandonrstone/league-of-legends-counterpicker";

type GithubRelease = {
  tag_name: string;
  assets: { name: string; browser_download_url: string }[];
};

function normalizeVersion(raw: string) {
  return raw.trim().replace(/^[vV]/, "");
}

export function versionIsNewer(remote: string, current: string) {
  const parse = (v: string) =>
    normalizeVersion(v)
      .split(".")
      .slice(0, 3)
      .map((p) => Number.parseInt(p, 10) || 0);
  const [a, b, c] = parse(remote);
  const [d, e, f] = parse(current);
  if (a !== d) return a > d;
  if (b !== e) return b > e;
  return c > f;
}

export function updateFromGithubRelease(release: GithubRelease, current: string): AppSnapshot["update"] {
  if (!versionIsNewer(release.tag_name, current)) return null;
  const asset =
    release.assets.find((a) => /setup\.exe$/i.test(a.name)) ??
    release.assets.find((a) => /\.exe$/i.test(a.name));
  if (!asset) return null;
  const version = normalizeVersion(release.tag_name);
  return {
    version,
    downloadUrl: asset.browser_download_url,
    assetName: asset.name.split(/[\\/]/).pop() || asset.name,
    status: "available",
    progress: 0,
    message: `Version ${version} is available`,
  };
}

export async function fetchGithubUpdate(current: string): Promise<AppSnapshot["update"]> {
  const res = await fetch(`https://api.github.com/repos/${GITHUB_REPO}/releases/latest`, {
    headers: { Accept: "application/vnd.github+json" },
  });
  if (!res.ok) return null;
  return updateFromGithubRelease(await res.json(), current);
}

export function previewCurrentVersion() {
  if (!import.meta.env.DEV) return null;
  return new URLSearchParams(window.location.search).get("asVersion");
}

export function previewEmptyState() {
  if (!import.meta.env.DEV) return null;
  return new URLSearchParams(window.location.search).get("empty");
}
