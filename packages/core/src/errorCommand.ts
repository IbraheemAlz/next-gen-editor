/* Issue #469 - which command an `Event::Error` answers.
 *
 * The engine stamps the refused command's WIRE name (`"APPLY_FORMATTING"`)
 * on `Event::Error.command`. A reply without the field (a pre-#469 engine,
 * an error not tied to a dispatched command, a delivered/mock event) falls
 * back to the legacy `<CommandName>: ` message prefix, normalised to the
 * same wire spelling so every consumer sees one format. Pure: no engine,
 * no DOM. */

/** The structural slice of `Event::Error` this needs. */
export interface ErrorEventLike {
    message: string;
    command?: string | undefined;
}

/** `ApplyFormatting` -> `APPLY_FORMATTING` (serde's SCREAMING_SNAKE_CASE). */
function toWireName(variant: string): string {
    return variant.replace(/(?<=[a-z0-9])(?=[A-Z])/g, '_').toUpperCase();
}

/** The wire name of the command an error answers, or `undefined`. */
export function errorCommand(evt: ErrorEventLike): string | undefined {
    if (evt.command !== undefined && evt.command !== '') return evt.command;
    const prefix = /^([A-Za-z][A-Za-z0-9]*): /.exec(evt.message);
    return prefix?.[1] !== undefined ? toWireName(prefix[1]) : undefined;
}
