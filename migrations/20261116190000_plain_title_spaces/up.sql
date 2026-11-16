-- Titles with every kind of whitespace as a plain space, as they are saved
-- from now on. A Mac types a no-break space for Option+Space, and Discord drops
-- one from a link's text. The set is Rust's char::is_whitespace.
UPDATE events
SET title = regexp_replace(title, E'[\t\n\u000B\f\r\u0085   -     　]', ' ', 'g')
WHERE title ~ E'[\t\n\u000B\f\r\u0085   -     　]';

UPDATE event_templates
SET title = regexp_replace(title, E'[\t\n\u000B\f\r\u0085   -     　]', ' ', 'g')
WHERE title ~ E'[\t\n\u000B\f\r\u0085   -     　]';
