import { ArrowsClockwiseIcon, HandIcon } from '@phosphor-icons/react';
import { cn } from '../lib/cn';

interface PipelineProgressProps {
  stage: string;
  status: string;
  stageIndex: number; // 1-based
  totalStages: number;
  attempt?: string; // "1/3" format
  awaitingApproval?: boolean;
  className?: string;
}

export function PipelineProgress({
  stage,
  status,
  stageIndex,
  totalStages,
  attempt,
  awaitingApproval,
  className,
}: PipelineProgressProps) {
  const dots = Array.from({ length: totalStages }, (_, i) => {
    const dotIndex = i + 1;
    const isCompleted = dotIndex < stageIndex;
    const isCurrent = dotIndex === stageIndex;

    return (
      <span
        key={i}
        className={cn(
          'inline-block w-1.5 h-1.5 rounded-full shrink-0',
          isCompleted && 'bg-success',
          isCurrent && status === 'running' && 'bg-brand',
          isCurrent && (status === 'paused' || awaitingApproval) && 'bg-brand',
          isCurrent && status === 'failed' && 'bg-error',
          !isCompleted && !isCurrent && 'bg-secondary',
        )}
      />
    );
  });

  // Stage display name: replace underscores, capitalize each word
  const stageName = stage
    .replace(/_/g, ' ')
    .replace(/\b\w/g, (c) => c.toUpperCase());

  const isRunning = status === 'running' && !awaitingApproval;
  const isPaused = awaitingApproval || status === 'paused';

  return (
    <div className={cn('flex flex-col gap-half', className)}>
      {/* Dots row */}
      <div className="flex items-center gap-1">
        {dots}
      </div>

      {/* Stage name + status icon */}
      <div className="flex items-center gap-half text-low">
        <span className="text-xs truncate">
          {stageName} ({stageIndex}/{totalStages})
        </span>

        {isRunning && (
          <ArrowsClockwiseIcon
            className="size-icon-2xs text-brand shrink-0 animate-spin"
            weight="bold"
          />
        )}

        {isPaused && (
          <HandIcon
            className="size-icon-2xs text-brand shrink-0"
            weight="fill"
          />
        )}

        {attempt && (
          <>
            <span className="text-low/50 shrink-0">·</span>
            <span className="text-xs whitespace-nowrap shrink-0">
              Attempt {attempt}
            </span>
          </>
        )}
      </div>
    </div>
  );
}
