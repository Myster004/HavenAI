/**
 * Normalized Discover character model.
 *
 * The UI only ever talks to this shape — never to the raw Chub API payload.
 * See chubMapper.ts for the Chub → DiscoverCharacter conversion.
 */
export interface DiscoverCharacter {
  id: string;
  projectId?: string;

  name: string;
  creator?: string;
  creatorUsername?: string;

  avatarUrl?: string;
  imageUrl?: string;

  tagline?: string;
  description?: string;

  tags: string[];

  rating?: number;
  ratingCount?: number;
  likes?: number;
  downloads?: number;
  views?: number;
  messages?: number;

  isNsfw?: boolean;
  tokenCount?: number;
  createdAt?: string;

  source: "chub";
  sourceUrl?: string;
  /** author/character path on Chub — used for detail + download */
  fullPath: string;

  raw?: unknown;
}

export interface DiscoverTag {
  name: string;
  count?: number;
}

export interface DiscoverSearchResult {
  characters: DiscoverCharacter[];
  page: number;
  totalPages?: number;
  totalResults?: number;
}

export interface ChubCharacterDetail extends DiscoverCharacter {
  personality?: string;
  scenario?: string;
  firstMessage?: string;
  exampleDialogue?: string;
  systemPrompt?: string;
  postHistoryInstructions?: string;
  creatorNotes?: string;
  alternateGreetings: string[];
  characterBookName?: string;
  characterBookEntryCount?: number;
}

export type DiscoverSort =
  | "trending"
  | "download_count_desc"
  | "star_count_desc"
  | "rating_desc"
  | "created_desc";

export const DISCOVER_PAGE_SIZE = 15;

export const DISCOVER_SORT_OPTIONS: { value: DiscoverSort; label: string }[] = [
  { value: "trending", label: "Trending" },
  { value: "download_count_desc", label: "Most Popular" },
  { value: "star_count_desc", label: "Most Liked" },
  { value: "rating_desc", label: "Top Rated" },
  { value: "created_desc", label: "Newest" },
];
