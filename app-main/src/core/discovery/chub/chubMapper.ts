import type {
  ChubCharacterDetail,
  DiscoverCharacter,
} from "./chubTypes";

type Json = Record<string, unknown>;

function asObject(value: unknown): Json {
  return value && typeof value === "object" ? (value as Json) : {};
}

/** Pick the first present, non-empty string (numbers coerced) from keys. */
function pickString(obj: Json, keys: string[]): string | undefined {
  for (const key of keys) {
    const value = obj[key];
    if (typeof value === "string" && value.trim().length > 0) return value;
    if (typeof value === "number" && Number.isFinite(value)) return String(value);
  }
  return undefined;
}

function pickNumber(obj: Json, keys: string[]): number | undefined {
  for (const key of keys) {
    const value = obj[key];
    if (typeof value === "number" && Number.isFinite(value) && value >= 0) return value;
  }
  return undefined;
}

function pickBool(obj: Json, keys: string[]): boolean | undefined {
  for (const key of keys) {
    const value = obj[key];
    if (typeof value === "boolean") return value;
  }
  return undefined;
}

function pickStringArray(obj: Json, key: string): string[] {
  const value = obj[key];
  if (!Array.isArray(value)) return [];
  return value
    .map((item) => {
      if (typeof item === "string" && item.trim().length > 0) return item;
      const inner = asObject(item);
      const name = pickString(inner, ["name"]);
      return name ?? "";
    })
    .filter((tag) => tag.length > 0);
}

function pickStringArrayFirst(obj: Json, keys: string[]): string[] {
  for (const key of keys) {
    const arr = pickStringArray(obj, key);
    if (arr.length > 0) return arr;
  }
  return [];
}

/**
 * Chub search/list node → normalized DiscoverCharacter.
 * Field fallbacks keep the mapper resilient to camelCase/snake_case drift.
 */
export function mapChubNodeToDiscoverCharacter(raw: unknown): DiscoverCharacter {
  const node = asObject(raw);
  const fullPath =
    pickString(node, ["fullPath", "full_path"]) ??
    [
      pickString(node, ["creator", "creatorName"]),
      pickString(node, ["name"]),
    ]
      .filter(Boolean)
      .join("/");

  const rating = pickNumber(node, ["rating"]);
  const ratingCount = pickNumber(node, ["ratingCount", "rating_count", "n_ratings"]);

  return {
    id: pickString(node, ["id"]) ?? fullPath,
    projectId: pickString(node, ["id"]),
    name: pickString(node, ["name"]) ?? "Unnamed Character",
    creator: pickString(node, ["creatorName", "creator"]),
    creatorUsername: pickString(node, ["creator"]),
    avatarUrl: pickString(node, ["avatarUrl", "avatar_url", "avatar"]),
    imageUrl: pickString(node, ["avatarUrl", "avatar_url", "avatar"]),
    tagline: pickString(node, ["tagline"]),
    description: pickString(node, ["description"]),
    tags: pickStringArrayFirst(node, ["tags", "topics", "labels"]),
    rating,
    ratingCount,
    likes: pickNumber(node, ["starCount", "star_count", "n_stars"]),
    downloads: pickNumber(node, ["downloadCount", "download_count"]),
    views: pickNumber(node, ["viewCount", "view_count"]),
    messages: pickNumber(node, ["messageCount", "message_count", "n_messages", "nMessages"]),
    isNsfw: pickBool(node, ["nsfw"]),
    tokenCount: pickNumber(node, ["nTokens", "n_tokens", "tokenCount"]),
    createdAt: pickString(node, ["createdAt", "created_at"]),
    source: "chub",
    sourceUrl: fullPath
      ? `https://chub.ai/characters/${fullPath}`
      : undefined,
    fullPath,
    raw,
  };
}

/**
 * Chub full character node → normalized detail model.
 * Only maps fields that actually exist in the payload; missing sections stay
 * undefined so the UI can hide them.
 */
export function mapChubDetailNode(raw: unknown): ChubCharacterDetail {
  const base = mapChubNodeToDiscoverCharacter(raw);
  const node = asObject(raw);

  const book = asObject(node["character_book"]);
  const bookEntries = Array.isArray(book["entries"]) ? (book["entries"] as unknown[]) : [];

  const alternateGreetings = Array.isArray(node["alternate_greetings"])
    ? (node["alternate_greetings"] as unknown[])
        .filter((g): g is string => typeof g === "string" && g.trim().length > 0)
    : [];

  return {
    ...base,
    personality: pickString(node, ["personality"]),
    scenario: pickString(node, ["scenario"]),
    firstMessage: pickString(node, ["first_mes", "firstMes", "greeting"]),
    exampleDialogue: pickString(node, ["mes_example", "mesExample", "example_dialogue"]),
    systemPrompt: pickString(node, ["system_prompt", "systemPrompt"]),
    postHistoryInstructions: pickString(node, [
      "post_history_instructions",
      "postHistoryInstructions",
    ]),
    creatorNotes: pickString(node, ["creator_notes", "creatorNotes"]),
    alternateGreetings,
    characterBookName: pickString(book, ["name"]),
    characterBookEntryCount:
      bookEntries.length > 0 ? bookEntries.length : undefined,
  };
}
