/**
 * "Me vs Them" speaker labels. The backend stores `speaker` as 'mic' (the user,
 * from the microphone) or 'system' (the other side of the call, from system audio).
 * Unlabelled transcripts (imported audio, older meetings) have no speaker.
 */
export function speakerLabel(speaker?: string | null): string | null {
  if (speaker === 'mic') return 'Me';
  if (speaker === 'system') return 'Them';
  return null;
}

/** Prefix text with "Me: " / "Them: " when the speaker is known. */
export function withSpeakerPrefix(text: string, speaker?: string | null): string {
  const label = speakerLabel(speaker);
  return label ? `${label}: ${text}` : text;
}
