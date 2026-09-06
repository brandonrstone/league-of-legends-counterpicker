import { useEffect, useMemo, useState, type MouseEvent } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { AppSnapshot, ChampionSlot, Recommendation } from "./types";
import { fetchGithubUpdate, previewCurrentVersion, previewEmptyState } from "./update";

const emptySnap = (): AppSnapshot => ({
  lcu: { connected: false, summonerName: null, gameName: null, detectedRank: null },
  gamePhase: null,
  draft: null,
  recommendations: [],
  stats: {
    ready: false,
    ingesting: false,
    stale: false,
    patch: null,
    source: "lolalytics",
    message: "Starting…",
    progress: 0,
  },
  settings: {
    rankBracket: "auto",
    ownedOnly: true,
    comfortWeighting: true,
    alwaysOnTop: true,
    roleOverride: "middle",
  },
  catalogReady: false,
  legal: "",
  version: "1.0.4",
  update: null,
});

const ROLES = [
  { id: "top", label: "Top" },
  { id: "jungle", label: "Jungle" },
  { id: "middle", label: "Mid" },
  { id: "bottom", label: "ADC" },
  { id: "support", label: "Support" },
];

function iconUrl(id: number) {
  return `https://raw.communitydragon.org/latest/plugins/rcp-be-lol-game-data/global/default/v1/champion-icons/${id}.png`;
}

function roleLabel(role: string) {
  return ROLES.find((r) => r.id === role)?.label ?? role;
}

function signed(n: number | null) {
  if (n == null) return "—";
  return `${n >= 0 ? "+" : ""}${n.toFixed(1)}%`;
}

function compactGames(n: number) {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 10_000) return `${Math.round(n / 1_000)}k`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
  return `${n}`;
}

function sampleLine(rec: Recommendation) {
  if (!rec.metaGames) return null;
  const parts = [`${compactGames(rec.metaGames)} games`];
  if (rec.metaPickrate) parts.push(`${rec.metaPickrate.toFixed(1)}% pick`);
  return parts.join(" · ");
}

export default function App() {
  const [snap, setSnap] = useState<AppSnapshot>(emptySnap);
  const [settingsOpen, setSettingsOpen] = useState(false);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    (async () => {
      try {
        setSnap(await invoke<AppSnapshot>("get_snapshot"));
      } catch {
        const asVersion = previewCurrentVersion();
        if (asVersion) {
          const update = await fetchGithubUpdate(asVersion).catch(() => null);
          if (update) {
            setSnap((prev) => ({ ...prev, update }));
          }
        }
        if (previewEmptyState() === "ready") {
          setSnap((prev) => ({
            ...prev,
            lcu: { ...prev.lcu, connected: true },
          }));
        }
      }
      unlisten = await listen<AppSnapshot>("snapshot", (event) => setSnap(event.payload));
    })();
    return () => unlisten?.();
  }, []);

  const phase = snap.gamePhase ?? "Idle";
  const inDraft = Boolean(snap.draft);
  const confidence = useMemo(() => {
    const n = snap.draft?.enemiesLocked ?? 0;
    return `${n}/5 enemies known`;
  }, [snap.draft?.enemiesLocked]);

  async function patchSettings(partial: Record<string, unknown>) {
    const next = await invoke<AppSnapshot>("update_settings", { patch: partial });
    setSnap(next);
  }

  async function onDownloadUpdate() {
    const update = snap.update;
    if (!update || update.status === "downloading") return;
    setSnap((prev) =>
      prev.update
        ? {
          ...prev,
          update: {
            ...prev.update,
            status: "downloading",
            message: `Downloading ${prev.update.version}…`,
          },
        }
        : prev,
    );
    try {
      await invoke("download_update");
    } catch {
      window.open(update.downloadUrl, "_blank", "noopener,noreferrer");
      setSnap((prev) =>
        prev.update
          ? {
            ...prev,
            update: {
              ...prev.update,
              status: "ready",
              progress: 1,
              message: "Download started",
            },
          }
          : prev,
      );
    }
  }

  async function onDismissUpdate(event: MouseEvent) {
    event.stopPropagation();
    event.preventDefault();
    try {
      const next = await invoke<AppSnapshot>("dismiss_update");
      setSnap(next);
    } catch {
      setSnap((prev) => ({ ...prev, update: null }));
    }
  }

  return (
    <div className="flex min-h-full flex-col px-5 py-5">
      <header className="mb-5 flex items-start justify-between gap-3">
        <div>
          <p className="text-[11px] font-medium tracking-[0.28em] text-gold uppercase">
            League of Legends
          </p>
          <h1 className="font-display text-[28px] font-medium leading-tight text-gold-2">Counterpicker</h1>
        </div>
        <button
          type="button"
          className="hex-frame flex h-9 w-9 cursor-pointer items-center justify-center rounded-full bg-[#132033]/70 text-gold hover:bg-[#1a2a40]"
          aria-label={settingsOpen ? "Close settings" : "Open settings"}
          aria-expanded={settingsOpen}
          onClick={() => setSettingsOpen((v) => !v)}
        >
          <GearIcon open={settingsOpen} />
        </button>
      </header>

      {snap.update ? (
        <UpdateBanner
          update={snap.update}
          onDownload={onDownloadUpdate}
          onDismiss={onDismissUpdate}
        />
      ) : null}

      <StatusBar snap={snap} phase={phase} settingsOpen={settingsOpen} />

      {settingsOpen ? (
        <SettingsPanel snap={snap} onPatch={patchSettings} />
      ) : null}

      {!snap.lcu.connected ? (
        <EmptyState
          title="Waiting for League Client"
          body="Launch the League client and get into a pre-game lobby. Counterpicker will automatically appear during champion selection."
          seeking
        />
      ) : !inDraft ? (
        <EmptyState
          title="Ready for champion select"
          body="Queue up. Once in a lobby, recommendations will update as champions are locked in, weighted by role and pick order."
          ready
        />
      ) : (
        <DraftBoard snap={snap} confidence={confidence} onPatch={patchSettings} />
      )}

      <footer className="mt-auto pt-5 text-[10px] leading-relaxed text-[#8b7d62]">
        {snap.legal}
      </footer>
    </div>
  );
}

function StatusBar({
  snap,
  phase,
  settingsOpen,
}: {
  snap: AppSnapshot;
  phase: string;
  settingsOpen: boolean;
}) {
  return (
    <div className="hex-frame mb-4 grid grid-cols-2 gap-3 rounded-2xl p-4 text-xs">
      <Stat
        label="Client"
        value={snap.lcu.connected ? snap.lcu.gameName || snap.lcu.summonerName || "Connected" : "Offline"}
      />
      <Stat label="Phase" value={phase} />
      <Stat label="Patch" value={snap.stats.patch ?? "—"} />
      <Stat
        label="Stats"
        value={
          snap.stats.ingesting
            ? `${Math.round(snap.stats.progress * 100)}%`
            : snap.stats.stale
              ? "Cached"
              : snap.stats.ready
                ? "Live"
                : "Loading"
        }
      />
      {settingsOpen ? <Stat label="App Version" value={snap.version} /> : null}
      <div
        className={`col-span-2 flex items-center gap-2 ${snap.stats.ingesting ? "text-gold-2" : "text-[#b9a888]"}`}
      >
        <span className="min-w-0 truncate">
          {snap.stats.ingesting
            ? `${snap.stats.message}  ·  ${Math.round(snap.stats.progress * 100)}%`
            : snap.stats.message}
        </span>
        {settingsOpen ? (
          <>
            <span aria-hidden="true" className="text-[#5f5642]">
              ·
            </span>
            <button
              type="button"
              className="refresh-action shrink-0"
              onClick={() => invoke("refresh_stats")}
              disabled={snap.stats.ingesting}
              title="Refresh stats"
              aria-label="Refresh stats"
            >
              <RefreshIcon />
            </button>
          </>
        ) : null}
      </div>
      {snap.stats.ingesting ? (
        <div className="col-span-2 h-1 overflow-hidden rounded-full bg-[#1b2c44]">
          <div
            className="h-full bg-gold transition-all"
            style={{ width: `${Math.min(100, snap.stats.progress * 100)}%` }}
          />
        </div>
      ) : null}
    </div>
  );
}

function UpdateBanner({
  update,
  onDownload,
  onDismiss,
}: {
  update: NonNullable<AppSnapshot["update"]>;
  onDownload: () => void;
  onDismiss: (event: MouseEvent) => void;
}) {
  const downloading = update.status === "downloading";
  return (
    <div className="hex-frame mb-4 flex items-start gap-2 rounded-2xl bg-[#1a1608]/55 p-4">
      <button
        type="button"
        className="update-action min-w-0 flex-1 text-left"
        onClick={onDownload}
        disabled={downloading}
      >
        <div className="update-action-title text-sm font-medium">{update.message}</div>
        <div className="update-action-body mt-0.5 text-[11px] leading-snug">
          {downloading
            ? "Saving the installer to your Downloads folder"
            : update.status === "ready"
              ? "Run the setup when it opens to upgrade in place"
              : update.status === "error"
                ? "Click to try again, or get it from GitHub Releases"
                : "Click to download the new installer and run it"}
        </div>
      </button>
      <button
        type="button"
        className="shrink-0 cursor-pointer px-1.5 text-base leading-none text-[#8b7d62] hover:text-gold-2"
        aria-label="Dismiss update"
        onClick={onDismiss}
      >
        ×
      </button>
    </div>
  );
}

function emptyRecsCopy(snap: AppSnapshot): string {
  const msg = snap.stats.message.trim();
  if (snap.stats.ingesting || !snap.stats.ready) {
    return msg || "Waiting for matchup stats to finish loading…";
  }
  if (
    snap.stats.stale ||
    /matchup tables|refresh failed|incomplete|cached role/i.test(msg)
  ) {
    return msg || "Matchup tables are missing. Try Refresh stats.";
  }
  const alreadyLocked = snap.draft?.allies.some((s) => s.isLocal && s.championId > 0);
  if (alreadyLocked) {
    return "No other in-role picks left to rank. Later enemy locks still update this list when stats are ready.";
  }
  return "No in-role picks left that you can still lock. Try another role or turn off Owned only.";
}

function Stat({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <div className="text-[10px] uppercase tracking-[0.18em] text-[#7d7159]">{label}</div>
      <div className="truncate text-gold-2">{value}</div>
    </div>
  );
}

function EmptyState({
  title,
  body,
  seeking = false,
  ready = false,
}: {
  title: string;
  body: string;
  seeking?: boolean;
  ready?: boolean;
}) {
  const ringMotion = seeking ? "animate-seek" : ready ? "animate-ready" : "";
  return (
    <section className="hex-frame flex flex-1 flex-col items-center justify-center rounded-2xl px-6 py-16 text-center">
      <div className="relative mb-5 h-16 w-16">
        <div
          className={`absolute inset-0 origin-center rounded-full border border-gold/35 bg-[#c8aa6e12] ${ringMotion}`}
        />
        <div
          className={`absolute inset-[14px] origin-center rounded-full border border-gold/70 ${ringMotion}${seeking ? " [animation-delay:600ms]" : ""}`}
        />
      </div>
      <h2 className="font-display text-xl font-medium text-gold">{title}</h2>
      <p className="mt-3 max-w-sm text-sm leading-relaxed text-[#cbb892]">{body}</p>
    </section>
  );
}

function DraftBoard({
  snap,
  confidence,
  onPatch,
}: {
  snap: AppSnapshot;
  confidence: string;
  onPatch: (p: Record<string, unknown>) => void;
}) {
  const draft = snap.draft!;
  return (
    <section className="flex flex-col gap-4">
      <div
        className={`hex-frame rounded-2xl p-4 ${draft.isOurTurn ? "pulse-gold border-gold/50" : ""}`}
      >
        <div className="flex items-center justify-between gap-2">
          <div>
            <div className="text-[10px] uppercase tracking-[0.2em] text-[#7d7159]">Your role</div>
            <div className="font-display text-lg font-medium text-gold">{roleLabel(draft.role)}</div>
          </div>
          <div className="text-right">
            <div className="text-[10px] uppercase tracking-[0.2em] text-[#7d7159]">Pick clock</div>
            <div className="text-lg text-gold-2">
              {draft.isOurTurn ? `${draft.secondsLeft}s · your pick` : `${draft.secondsLeft}s`}
            </div>
          </div>
        </div>
        <div className="mt-2 flex flex-wrap gap-1">
          {ROLES.map((role) => (
            <button
              key={role.id}
              onClick={() => onPatch({ roleOverride: role.id })}
              className={`rounded-full px-2.5 py-1 text-[11px] font-medium ${draft.role === role.id ? "bg-gold text-[#0a1428]" : "bg-[#1a2a40]/80 text-[#d7c7a4]"
                }`}
            >
              {role.label}
            </button>
          ))}
        </div>
        <div className="mt-2 text-xs text-[#b9a888]">{confidence}</div>
      </div>

      <TeamRow title="Allies" slots={draft.allies} ally />
      <TeamRow title="Enemies" slots={draft.enemies} ally={false} />

      <div>
        <h2 className="mb-2 text-sm font-medium tracking-[0.14em] text-gold uppercase">
          Best available picks
        </h2>
        <div className="flex flex-col gap-2">
          {snap.recommendations.length === 0 ? (
            <div className="hex-frame rounded-2xl p-4 text-sm text-[#cbb892]">
              {emptyRecsCopy(snap)}
            </div>
          ) : (
            snap.recommendations.map((rec, i) => (
              <RecCard key={rec.championId} rec={rec} rank={i + 1} active={draft.isOurTurn && i === 0} />
            ))
          )}
        </div>
      </div>
    </section>
  );
}

function TeamRow({ title, slots, ally }: { title: string; slots: ChampionSlot[]; ally: boolean }) {
  const filled = slots.length ? slots : Array.from({ length: 5 }, (_, i) => ({
    cellId: i,
    championId: 0,
    intentId: 0,
    assignedPosition: "",
    displayChampionId: 0,
    isLocal: false,
  }));
  return (
    <div>
      <div className="mb-1 text-[10px] uppercase tracking-[0.2em] text-[#7d7159]">{title}</div>
      <div className="flex gap-2">
        {filled.map((slot) => (
          <div
            key={`${title}-${slot.cellId}`}
            className={`hex-frame flex h-14 w-14 items-center justify-center overflow-hidden rounded-xl ${ally ? "bg-[#10283a]/70" : "bg-[#2a1418]/70"
              } ${slot.isLocal ? "ring-1 ring-gold/70" : ""}`}
            title={slot.assignedPosition}
          >
            {slot.displayChampionId > 0 ? (
              <img
                src={iconUrl(slot.displayChampionId)}
                alt=""
                className="h-full w-full object-cover"
              />
            ) : (
              <span className="text-[10px] text-[#6d614c]">—</span>
            )}
          </div>
        ))}
      </div>
    </div>
  );
}

function RecCard({ rec, rank, active }: { rec: Recommendation; rank: number; active: boolean }) {
  const sample = sampleLine(rec);
  return (
    <article
      className={`hex-frame flex gap-3 rounded-2xl p-3 ${active ? "pulse-gold" : ""}`}
    >
      <div className="relative h-14 w-14 overflow-hidden rounded-xl border border-gold/25">
        <img src={rec.iconUrl} alt={rec.name} className="h-full w-full object-cover" />
        <span className="absolute left-0 top-0 bg-gold px-1.5 text-[10px] font-bold text-[#0a1428]">
          {rank}
        </span>
      </div>
      <div className="min-w-0 flex-1">
        <div className="flex items-baseline justify-between gap-2">
          <h3 className="truncate font-display text-base font-medium text-gold-2">{rec.name}</h3>
          <span className="text-xs text-gold">{rec.score.toFixed(2)}</span>
        </div>
        <p className="mt-0.5 text-xs leading-snug text-[#d2c3a0]">{rec.reason}</p>
        <div className="mt-1 flex gap-3 text-[10px] uppercase tracking-wide text-[#8d8068]">
          <span>Lane {signed(rec.laneDelta)}</span>
          <span>Team {signed(rec.teamDelta)}</span>
          <span>WR {rec.metaWr?.toFixed(1) ?? "—"}%</span>
        </div>
        {sample ? <div className="mt-0.5 text-[10px] text-[#7d7159]">{sample}</div> : null}
      </div>
    </article>
  );
}

function GearIcon({ open }: { open: boolean }) {
  return (
    <svg
      className={`gear-icon h-5 w-5 ${open ? "is-open" : ""}`}
      viewBox="0 0 24 24"
      fill="currentColor"
      aria-hidden="true"
    >
      <path d="M19.14 12.94c.04-.31.06-.63.06-.94s-.02-.63-.06-.94l2.03-1.58a.5.5 0 0 0 .12-.64l-1.92-3.32a.5.5 0 0 0-.6-.22l-2.39.96c-.5-.38-1.03-.7-1.62-.94l-.36-2.54a.5.5 0 0 0-.5-.42h-3.84a.5.5 0 0 0-.5.42l-.36 2.54c-.59.24-1.13.57-1.62.94l-2.39-.96a.5.5 0 0 0-.6.22L2.71 8.84a.5.5 0 0 0 .12.64l2.03 1.58c-.04.31-.06.63-.06.94s.02.63.06.94l-2.03 1.58a.5.5 0 0 0-.12.64l1.92 3.32c.13.23.4.32.64.22l2.39-.96c.5.38 1.03.7 1.62.94l.36 2.54c.05.24.26.42.5.42h3.84c.24 0 .45-.18.5-.42l.36-2.54c.59-.24 1.13-.57 1.62-.94l2.39.96c.24.1.51 0 .64-.22l1.92-3.32a.5.5 0 0 0-.12-.64l-2.03-1.58zM12 15.6A3.6 3.6 0 1 1 12 8.4a3.6 3.6 0 0 1 0 7.2z" />
    </svg>
  );
}

function RefreshIcon() {
  return (
    <svg
      className="h-3.5 w-3.5"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d="M20 11.5a8 8 0 0 0-13.7-5.2L3 9.2" />
      <path d="M4 12.5a8 8 0 0 0 13.7 5.2L21 14.8" />
      <path d="M3 4.5v4.7h4.7" />
      <path d="M21 19.5v-4.7h-4.7" />
    </svg>
  );
}

function SettingsToggle({
  checked,
  onChange,
  title,
  description,
  last = false,
}: {
  checked: boolean;
  onChange: (next: boolean) => void;
  title: string;
  description: string;
  last?: boolean;
}) {
  return (
    <label
      className={`flex items-start gap-3 text-xs text-[#d2c3a0] ${last ? "" : "mb-3"}`}
    >
      <input
        type="checkbox"
        className="switch mt-0.5"
        checked={checked}
        onChange={(e) => onChange(e.target.checked)}
      />
      <span>
        <span className="block">{title}</span>
        <span className="mt-0.5 block text-[10px] leading-snug text-[#8b7d62]">{description}</span>
      </span>
    </label>
  );
}

function SettingsPanel({
  snap,
  onPatch,
}: {
  snap: AppSnapshot;
  onPatch: (p: Record<string, unknown>) => void;
}) {
  return (
    <section className="hex-frame mb-4 rounded-2xl p-4 text-sm">
      <h2 className="mb-3 text-sm font-medium tracking-[0.14em] text-gold uppercase">Settings</h2>
      <label className="mb-3 block text-xs text-[#b9a888]">
        Rank bracket
        <select
          className="mt-1.5 w-full cursor-pointer rounded-xl border border-gold/20 bg-[#102036]/80 p-2.5 text-gold-2"
          value={snap.settings.rankBracket}
          onChange={(e) => onPatch({ rankBracket: e.target.value })}
        >
          <option value="auto">Auto from rank</option>
          <option value="diamond_plus">Diamond+</option>
          <option value="emerald_plus">Emerald+</option>
          <option value="platinum_plus">Platinum+</option>
          <option value="gold_plus">Gold+</option>
          <option value="silver">Silver</option>
          <option value="bronze">Bronze</option>
          <option value="iron">Iron</option>
          <option value="all">All ranks</option>
        </select>
      </label>
      <SettingsToggle
        checked={snap.settings.ownedOnly}
        onChange={(v) => onPatch({ ownedOnly: v })}
        title="Owned champions only"
        description="Hide champs that are not in your collection, so the list only suggests what you can actually lock."
      />
      <SettingsToggle
        checked={snap.settings.comfortWeighting}
        onChange={(v) => onPatch({ comfortWeighting: v })}
        title="Weight by champion mastery"
        description="Leans the list toward champs you have played, and stops niche picks from being discounted when you are the one-trick. Lane and team matchups still matter more."
      />
      <SettingsToggle
        checked={snap.settings.alwaysOnTop}
        onChange={(v) => onPatch({ alwaysOnTop: v })}
        title="Always on top"
        description="Keeps this overlay above the League client so you can see picks during champ select. Turn it off if the window is covering the client and you would rather Alt-Tab to it."
        last
      />
    </section>
  );
}
