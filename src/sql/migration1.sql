-- Shared drive information
create table drives (
  key           serial      primary key
  -- Drive API shared drive resource ID.
, id            text        not null unique
  -- Time of drive creation.
, created_time  timestamp   not null default (timezone('utc', now()))
);

create index ix_drives_id on drives (id);
create index ix_drives_created_time on drives (created_time);

-- File information
create table files (
  key           serial      primary key
  -- Drive API file resource ID.
, id            text        not null unique
  -- Key of the containing drive.
, drive_key     integer     not null references drives on delete cascade
  -- Original size before encryption.
, size          bigint      not null
  -- File content type.
, content_type  text        not null
  -- Time of file creation.
, created_time  timestamp   not null default (timezone('utc', now()))
  -- Time of last file access.
, accessed_time timestamp   not null default (timezone('utc', now()))
  -- Encrypted file secret for decryption.
, secret        bytea       not null
);

create index ix_files_id on files (id);
create index ix_files_drive_key on files (drive_key);
create index ix_files_size on files (size);
create index ix_files_content_type on files (content_type);
create index ix_files_created_time on files (created_time);
create index ix_files_accessed_time on files (accessed_time);
