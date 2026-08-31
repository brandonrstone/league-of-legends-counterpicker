import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { AppSnapshot, ChampionSlot, Recommendation } from "./types";

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
    riotPlatform: "na1",
    hasRiotKey: false,
  },
  catalogReady: false,
  legal: "",
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

export default function App() {
  const [snap, setSnap] = useState<AppSnapshot>(emptySnap);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [riotKey, setRiotKey] = useState("");

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    (async () => {
      try {
        setSnap(await invoke<AppSnapshot>("get_snapshot"));
      } catch {
        /* backend still booting */
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

  return (
    <div className="flex min-h-full flex-col px-4 py-4">
      <header className="mb-4 flex items-start justify-between gap-3">
        <div>
          <p className="font-display text-[11px] tracking-[0.28em] text-gold uppercase">
            Pre-game companion
          </p>
          <h1 className="font-display text-2xl font-bold text-gold-2">Rift Counterpick</h1>
        </div>
        <button
          className="hex-frame rounded-sm bg-[#132033] px-3 py-1.5 text-xs tracking-wide text-gold"
          onClick={() => setSettingsOpen((v) => !v)}
        >
          {settingsOpen ? "Close" : "Settings"}
        </button>
      </header>

      <StatusBar snap={snap} phase={phase} />

      {settingsOpen ? (
        <SettingsPanel
          snap={snap}
          riotKey={riotKey}
          setRiotKey={setRiotKey}
          onPatch={patchSettings}
        />
      ) : null}

      {!snap.lcu.connected ? (
        <EmptyState
          title="Waiting for League Client"
          body="Open the League client and stay in a lobby. This app reads live champion select from your PC — it cannot work as a website."
        />
      ) : !inDraft ? (
        <EmptyState
          title="Ready for champion select"
          body="Queue up. Recommendations update as each champion locks, weighted by your pick order and role."
        />
      ) : (
        <DraftBoard snap={snap} confidence={confidence} onPatch={patchSettings} />
      )}

      <footer className="mt-auto pt-4 text-[10px] leading-relaxed text-[#8b7d62]">
        {snap.legal}
      </footer>
    </div>
  );
}

function StatusBar({ snap, phase }: { snap: AppSnapshot; phase: string }) {
  return (
    <div className="hex-frame mb-4 grid grid-cols-2 gap-2 rounded-sm bg-[#0c1828]/80 p-3 text-xs">
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
      <div className="col-span-2 text-[#b9a888]">{snap.stats.message}</div>
      {snap.stats.ingesting ? (
        <div className="col-span-2 h-1 overflow-hidden rounded bg-[#1b2c44]">
          <div
            className="h-full bg-gold transition-all"
            style={{ width: `${Math.min(100, snap.stats.progress * 100)}%` }}
          />
        </div>
      ) : null}
    </div>
  );
}

function Stat({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <div className="text-[10px] uppercase tracking-[0.18em] text-[#7d7159]">{label}</div>
      <div className="truncate text-gold-2">{value}</div>
    </div>
  );
}

function EmptyState({ title, body }: { title: string; body: string }) {
  return (
    <section className="hex-frame flex flex-1 flex-col items-center justify-center rounded-sm bg-[#0c1828]/70 px-6 py-16 text-center">
      <div className="mb-4 h-16 w-16 rotate-45 border border-gold/70 bg-[#c8aa6e22]" />
      <h2 className="font-display text-xl text-gold">{title}</h2>
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
        className={`hex-frame rounded-sm bg-[#0c1828]/85 p-3 ${draft.isOurTurn ? "pulse-gold border-gold" : ""}`}
      >
        <div className="flex items-center justify-between gap-2">
          <div>
            <div className="text-[10px] uppercase tracking-[0.2em] text-[#7d7159]">Your role</div>
            <div className="font-display text-lg text-gold">{roleLabel(draft.role)}</div>
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
              className={`rounded-sm px-2 py-1 text-[11px] ${draft.role === role.id ? "bg-gold text-[#0a1428]" : "bg-[#1a2a40] text-[#d7c7a4]"
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
        <h2 className="mb-2 font-display text-sm tracking-[0.14em] text-gold uppercase">
          Best available picks
        </h2>
        <div className="flex flex-col gap-2">
          {snap.recommendations.length === 0 ? (
            <div className="hex-frame rounded-sm bg-[#0c1828]/80 p-4 text-sm text-[#cbb892]">
              {snap.stats.ready
                ? "No in-role picks left that you can still lock. Try another role or turn off Owned only."
                : "Waiting for matchup stats to finish loading…"}
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
            className={`hex-frame flex h-14 w-14 items-center justify-center overflow-hidden rounded-sm ${ally ? "bg-[#10283a]" : "bg-[#2a1418]"
              } ${slot.isLocal ? "ring-1 ring-gold" : ""}`}
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
  return (
    <article
      className={`hex-frame flex gap-3 rounded-sm bg-[#0c1828]/90 p-2.5 ${active ? "pulse-gold" : ""}`}
    >
      <div className="relative h-14 w-14 overflow-hidden rounded-sm border border-gold/40">
        <img src={rec.iconUrl} alt={rec.name} className="h-full w-full object-cover" />
        <span className="absolute left-0 top-0 bg-gold px-1.5 text-[10px] font-bold text-[#0a1428]">
          {rank}
        </span>
      </div>
      <div className="min-w-0 flex-1">
        <div className="flex items-baseline justify-between gap-2">
          <h3 className="truncate font-display text-base text-gold-2">{rec.name}</h3>
          <span className="text-xs text-gold">{rec.score.toFixed(2)}</span>
        </div>
        <p className="mt-0.5 text-xs leading-snug text-[#d2c3a0]">{rec.reason}</p>
        <div className="mt-1 flex gap-3 text-[10px] uppercase tracking-wide text-[#8d8068]">
          <span>Lane {signed(rec.laneDelta)}</span>
          <span>Team {signed(rec.teamDelta)}</span>
          <span>WR {rec.metaWr?.toFixed(1) ?? "—"}%</span>
        </div>
      </div>
    </article>
  );
}

function SettingsPanel({
  snap,
  riotKey,
  setRiotKey,
  onPatch,
}: {
  snap: AppSnapshot;
  riotKey: string;
  setRiotKey: (v: string) => void;
  onPatch: (p: Record<string, unknown>) => void;
}) {
  return (
    <section className="hex-frame mb-4 rounded-sm bg-[#0c1828] p-3 text-sm">
      <h2 className="mb-3 font-display text-gold">Settings</h2>
      <label className="mb-2 block text-xs text-[#b9a888]">
        Rank bracket
        <select
          className="mt-1 w-full rounded-sm border border-gold/30 bg-[#102036] p-2 text-gold-2"
          value={snap.settings.rankBracket}
          onChange={(e) => onPatch({ rankBracket: e.target.value })}
        >
          <option value="auto">Auto from rank</option>
          <option value="emerald_plus">Emerald+</option>
          <option value="diamond_plus">Diamond+</option>
          <option value="platinum_plus">Platinum+</option>
          <option value="gold_plus">Gold+</option>
          <option value="all">All ranks</option>
        </select>
      </label>
      <label className="mb-2 flex items-center gap-2 text-xs text-[#d2c3a0]">
        <input
          type="checkbox"
          checked={snap.settings.ownedOnly}
          onChange={(e) => onPatch({ ownedOnly: e.target.checked })}
        />
        Owned champions only
      </label>
      <label className="mb-2 flex items-center gap-2 text-xs text-[#d2c3a0]">
        <input
          type="checkbox"
          checked={snap.settings.comfortWeighting}
          onChange={(e) => onPatch({ comfortWeighting: e.target.checked })}
        />
        Weight by champion mastery
      </label>
      <label className="mb-3 flex items-center gap-2 text-xs text-[#d2c3a0]">
        <input
          type="checkbox"
          checked={snap.settings.alwaysOnTop}
          onChange={(e) => onPatch({ alwaysOnTop: e.target.checked })}
        />
        Always on top
      </label>
      <label className="mb-2 block text-xs text-[#b9a888]">
        Riot platform
        <input
          className="mt-1 w-full rounded-sm border border-gold/30 bg-[#102036] p-2 text-gold-2"
          value={snap.settings.riotPlatform}
          onChange={(e) => onPatch({ riotPlatform: e.target.value })}
        />
      </label>
      <label className="mb-2 block text-xs text-[#b9a888]">
        Optional Riot API key (stored locally, never logged)
        <input
          type="password"
          className="mt-1 w-full rounded-sm border border-gold/30 bg-[#102036] p-2 text-gold-2"
          placeholder={snap.settings.hasRiotKey ? "Key saved — paste to replace" : "RGAPI-…"}
          value={riotKey}
          onChange={(e) => setRiotKey(e.target.value)}
        />
      </label>
      <div className="flex gap-2">
        <button
          className="rounded-sm bg-gold px-3 py-1.5 text-xs font-semibold text-[#0a1428]"
          onClick={() => {
            if (riotKey.trim()) {
              onPatch({ riotApiKey: riotKey.trim() });
              setRiotKey("");
            }
          }}
        >
          Save key
        </button>
        <button
          className="rounded-sm border border-gold/40 px-3 py-1.5 text-xs text-gold"
          onClick={() => invoke("refresh_stats")}
        >
          Refresh stats
        </button>
      </div>
    </section>
  );
}
