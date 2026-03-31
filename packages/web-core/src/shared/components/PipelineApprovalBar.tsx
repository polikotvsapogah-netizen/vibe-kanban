import { useCallback, useState, type MouseEvent } from 'react';
import { Button } from '@vibe/ui/components/Button';
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from '@vibe/ui/components/RadixTooltip';
import { pipelineApi } from '@/shared/lib/api';
import { Check, X } from 'lucide-react';
import { useTranslation } from 'react-i18next';

interface PipelineApprovalBarProps {
  workspaceId: string;
  stageName: string;
  onApprove?: () => void;
  onReject?: () => void;
}

export function PipelineApprovalBar({
  workspaceId,
  stageName,
  onApprove,
  onReject,
}: PipelineApprovalBarProps) {
  const { t } = useTranslation('pipeline');
  const [isApproving, setIsApproving] = useState(false);
  const [isRejecting, setIsRejecting] = useState(false);
  const [showFeedback, setShowFeedback] = useState(false);
  const [feedback, setFeedback] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [hasResponded, setHasResponded] = useState(false);

  const disabled = isApproving || isRejecting || hasResponded;

  const stopCardNavigation = useCallback((event: MouseEvent<HTMLElement>) => {
    event.stopPropagation();
  }, []);

  const handleApprove = useCallback(async () => {
    if (disabled) return;
    setIsApproving(true);
    setError(null);

    try {
      await pipelineApi.approve(workspaceId);
      setHasResponded(true);
      onApprove?.();
    } catch (e: unknown) {
      const errorMessage = e instanceof Error ? e.message : 'Failed to approve';
      setError(errorMessage);
    } finally {
      setIsApproving(false);
    }
  }, [disabled, workspaceId, onApprove]);

  const handleStartReject = useCallback(() => {
    if (disabled) return;
    setError(null);
    setShowFeedback(true);
  }, [disabled]);

  const handleCancelReject = useCallback(() => {
    if (isRejecting) return;
    setShowFeedback(false);
    setFeedback('');
  }, [isRejecting]);

  const handleSubmitReject = useCallback(async () => {
    if (disabled) return;
    setIsRejecting(true);
    setError(null);

    try {
      const trimmed = feedback.trim();
      await pipelineApi.reject(workspaceId, trimmed || undefined);
      setHasResponded(true);
      setShowFeedback(false);
      onReject?.();
    } catch (e: unknown) {
      const errorMessage = e instanceof Error ? e.message : 'Failed to reject';
      setError(errorMessage);
    } finally {
      setIsRejecting(false);
    }
  }, [disabled, feedback, workspaceId, onReject]);

  if (hasResponded) return null;

  // Stage display name: replace underscores, capitalize each word
  const displayStageName = stageName
    .replace(/_/g, ' ')
    .replace(/\b\w/g, (c) => c.toUpperCase());

  return (
    <div
      className="bg-background border border-border/60 rounded-sm px-2 py-1.5 text-xs sm:text-sm"
      onClick={stopCardNavigation}
    >
      <TooltipProvider>
        <div className="flex items-center justify-between gap-1.5 pl-4">
          <div className="flex items-center gap-1.5">
            {!showFeedback && (
              <span className="text-muted-foreground">
                {t('awaitingApproval')}: {displayStageName}
              </span>
            )}
          </div>
          {!showFeedback && (
            <div className="flex items-center gap-1.5 pr-4">
              <Tooltip>
                <TooltipTrigger asChild>
                  <Button
                    onClick={handleApprove}
                    variant="ghost"
                    className="h-8 w-8 rounded-full p-0 text-green-600 hover:text-green-700 hover:bg-green-50"
                    disabled={disabled}
                    aria-label={
                      isApproving ? t('submittingApproval') : t('approve')
                    }
                    aria-busy={isApproving}
                  >
                    <Check className="h-5 w-5" />
                  </Button>
                </TooltipTrigger>
                <TooltipContent>
                  <p>{isApproving ? t('submitting') : t('approve')}</p>
                </TooltipContent>
              </Tooltip>

              <Tooltip>
                <TooltipTrigger asChild>
                  <Button
                    onClick={handleStartReject}
                    variant="ghost"
                    className="h-8 w-8 rounded-full p-0"
                    disabled={disabled}
                    aria-label={
                      isRejecting ? t('submittingRejection') : t('reject')
                    }
                    aria-busy={isRejecting}
                  >
                    <X className="h-5 w-5" />
                  </Button>
                </TooltipTrigger>
                <TooltipContent>
                  <p>{isRejecting ? t('submitting') : t('reject')}</p>
                </TooltipContent>
              </Tooltip>
            </div>
          )}
        </div>

        {error && (
          <div
            className="mt-1 text-xs text-red-600"
            role="alert"
            aria-live="polite"
          >
            {error}
          </div>
        )}

        {showFeedback && !hasResponded && (
          <div className="flex flex-col gap-2 p-4">
            <textarea
              value={feedback}
              onChange={(e) => setFeedback(e.target.value)}
              placeholder={t('feedbackPlaceholder')}
              disabled={isRejecting}
              className="min-h-[80px] w-full rounded-sm border border-input bg-transparent px-3 py-2 text-sm ring-offset-background placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:cursor-not-allowed disabled:opacity-50"
            />
            <div className="flex flex-wrap items-center justify-end gap-2">
              <Button
                variant="ghost"
                size="sm"
                onClick={handleCancelReject}
                disabled={isRejecting}
              >
                {t('cancel')}
              </Button>
              <Button
                size="sm"
                onClick={handleSubmitReject}
                disabled={isRejecting}
              >
                {t('reject')}
              </Button>
            </div>
          </div>
        )}
      </TooltipProvider>
    </div>
  );
}
