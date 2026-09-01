export type ChampionSlot = {
  cellId: number;
  championId: number;
  intentId: number;
  assignedPosition: string;
  displayChampionId: number;
  isLocal: boolean;
};

export type DraftView = {
  role: string;
  pickTurn: number;
  isOurTurn: boolean;
  phase: string;
  secondsLeft: number;
  allies: ChampionSlot[];
  enemies: ChampionSlot[];
  bans: number[];
  enemiesLocked: number;
  alliesLocked: number;
  laneEnemyId: number | null;
};

export type Recommendation = {
  championId: number;
  name: string;
  slug: string;
  iconUrl: string;
  score: number;
  reason: string;
  laneDelta: number | null;
  teamDelta: number | null;
  synergyDelta: number | null;
  metaWr: number | null;
};

export type AppSnapshot = {
  lcu: {
    connected: boolean;
    summonerName: string | null;
    gameName: string | null;
    detectedRank: string | null;
  };
  gamePhase: string | null;
  draft: DraftView | null;
  recommendations: Recommendation[];
  stats: {
    ready: boolean;
    ingesting: boolean;
    stale: boolean;
    patch: string | null;
    source: string;
    message: string;
    progress: number;
  };
  settings: {
    rankBracket: string;
    ownedOnly: boolean;
    comfortWeighting: boolean;
    alwaysOnTop: boolean;
    roleOverride: string;
  };
  catalogReady: boolean;
  legal: string;
};
