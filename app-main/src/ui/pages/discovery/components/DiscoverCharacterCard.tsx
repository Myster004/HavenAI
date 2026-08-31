import { memo, useEffect, useState } from "react";
import { Download, Heart, Shield } from "lucide-react";
import { cn } from "../../../design-tokens";
import { formatCount } from "../../../../core/discovery/chub/chubApi";
import type { DiscoverCharacter } from "../../../../core/discovery/chub/chubTypes";

interface DiscoverCharacterCardProps {
  character: DiscoverCharacter;
  showNsfw?: boolean;
  onClick: (character: DiscoverCharacter) => void;
}

export const DiscoverCharacterCard = memo(function DiscoverCharacterCard({
  character,
  showNsfw = false,
  onClick,
}: DiscoverCharacterCardProps) {
  const [imageLoaded, setImageLoaded] = useState(false);
  const [imageError, setImageError] = useState(false);

  useEffect(() => {
    setImageLoaded(false);
    setImageError(false);
  }, [character.avatarUrl]);

  const blurNsfw = character.isNsfw && !showNsfw;

  const gradientHue =
    character.name
      .split("")
      .reduce((acc, char) => acc + char.charCodeAt(0), 0) % 360;
  const fallbackGradient = `linear-gradient(160deg, hsl(${gradientHue}, 45%, 18%) 0%, hsl(${(gradientHue + 50) % 360}, 35%, 12%) 100%)`;

  return (
    <button
      type="button"
      onClick={() => onClick(character)}
      className={cn(
        "group flex cursor-pointer flex-col overflow-hidden rounded-xl border text-left",
        "border-fg/10 bg-fg/[0.02] transition-all duration-200",
        "hover:border-fg/25 hover:bg-fg/[0.04] active:scale-[0.98]",
      )}
    >
      {/* Image */}
      <div
        className="relative aspect-3/4 w-full overflow-hidden"
        style={{ background: fallbackGradient }}
      >
        {character.avatarUrl && !imageError && (
          <img
            src={character.avatarUrl}
            alt={character.name}
            loading="lazy"
            decoding="async"
            onLoad={() => setImageLoaded(true)}
            onError={() => setImageError(true)}
            className={cn(
              "absolute inset-0 h-full w-full object-cover transition-all duration-500",
              "group-hover:scale-105",
              imageLoaded ? "opacity-100" : "opacity-0",
              blurNsfw && "blur-xl",
            )}
          />
        )}

        {/* Skeleton while loading */}
        {!imageLoaded && !imageError && (
          <div className="absolute inset-0 animate-pulse bg-linear-to-br from-white/5 to-white/10" />
        )}

        {/* NSFW overlay */}
        {blurNsfw && (
          <div className="absolute inset-0 z-10 flex items-center justify-center bg-black/50">
            <Shield className="h-8 w-8 text-danger" />
          </div>
        )}

        {/* Badges */}
        <div className="absolute left-2 right-2 top-2 z-20 flex items-start justify-between">
          {character.isNsfw && (
            <span className="flex items-center gap-1 rounded-full bg-danger/90 px-2 py-0.5 text-[9px] font-bold uppercase tracking-wider text-fg shadow-lg">
              <Shield className="h-2.5 w-2.5" />
              NSFW
            </span>
          )}
          {character.likes !== undefined && character.likes > 0 && (
            <span className="ml-auto flex items-center gap-1 rounded-full bg-black/50 px-2 py-0.5 text-[10px] font-semibold text-fg backdrop-blur-md">
              <Heart className="h-3 w-3 fill-danger text-danger" />
              {formatCount(character.likes)}
            </span>
          )}
        </div>

        {/* Bottom gradient for readability */}
        <div className="pointer-events-none absolute inset-x-0 bottom-0 z-10 h-12 bg-linear-to-t from-black/50 to-transparent" />
      </div>

      {/* Info */}
      <div className="flex flex-col gap-1 p-2.5">
        <div className="flex items-center justify-between gap-1">
          <h3 className="line-clamp-1 text-sm font-semibold text-fg">
            {character.name}
          </h3>
          <Download className="h-3.5 w-3.5 shrink-0 text-fg/25 transition-colors group-hover:text-fg/60" />
        </div>

        {character.creator && (
          <span className="text-[11px] text-fg/50">
            @{character.creatorUsername ?? character.creator}
          </span>
        )}

        {character.tagline && (
          <p className="line-clamp-2 text-[11px] leading-relaxed text-fg/60">
            {character.tagline}
          </p>
        )}

        {character.tags.length > 0 && (
          <div className="mt-0.5 flex flex-wrap gap-1">
            {character.tags.slice(0, 3).map((tag) => (
              <span
                key={tag}
                className="rounded-full bg-fg/10 px-2 py-0.5 text-[9px] font-medium text-fg/70"
              >
                {tag}
              </span>
            ))}
            {character.tags.length > 3 && (
              <span className="rounded-full bg-fg/5 px-1.5 py-0.5 text-[9px] font-medium text-fg/40">
                +{character.tags.length - 3}
              </span>
            )}
          </div>
        )}
      </div>
    </button>
  );
});

export default DiscoverCharacterCard;
