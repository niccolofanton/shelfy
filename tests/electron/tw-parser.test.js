import twParser from '../../electron/tw-parser.js';

const { extractTweet, parseBookmarkResponse, normalizeExportedPost } = twParser;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function makeTweetResult({ restId = '123456', screenName = 'alice', name = 'Alice A', fullText = 'Hello', createdAt = 'Mon Jan 01 00:00:00 +0000 2024', mediaEntities = [] } = {}) {
  return {
    rest_id: restId,
    legacy: {
      id_str: restId,
      full_text: fullText,
      created_at: createdAt,
      extended_entities: mediaEntities.length ? { media: mediaEntities } : undefined,
    },
    core: {
      user_results: {
        result: {
          legacy: { screen_name: screenName, name },
        },
      },
    },
  };
}

function makeImageMedia(url = 'https://pbs.twimg.com/media/img.jpg') {
  return { type: 'photo', media_url_https: url };
}

function makeVideoMedia(url = 'https://pbs.twimg.com/media/vid.jpg') {
  return { type: 'video', media_url_https: url };
}

function makeGifMedia(url = 'https://pbs.twimg.com/media/gif.jpg') {
  return { type: 'animated_gif', media_url_https: url };
}

// ---------------------------------------------------------------------------
// extractTweet
// ---------------------------------------------------------------------------

describe('extractTweet', () => {
  it('returns a well-formed item for a tweet with a single image', () => {
    const media = makeImageMedia('https://pbs.twimg.com/media/photo.jpg');
    const result = makeTweetResult({ mediaEntities: [media] });

    const item = extractTweet(result);

    expect(item).not.toBeNull();
    expect(item.id).toBe('123456');
    expect(item.platform).toBe('twitter');
    expect(item.postUrl).toBe('https://x.com/alice/status/123456');
    expect(item.profileUrl).toBe('https://x.com/alice');
    expect(item.authorUsername).toBe('alice');
    expect(item.authorName).toBe('Alice A');
    expect(item.text).toBe('Hello');
    expect(item.mediaType).toBe('image');
    expect(item.thumbnailUrl).toBe('https://pbs.twimg.com/media/photo.jpg');
    expect(item.shortcode).toBe('');
  });

  it('detects video mediaType', () => {
    const result = makeTweetResult({ mediaEntities: [makeVideoMedia()] });
    expect(extractTweet(result).mediaType).toBe('video');
  });

  it('detects animated_gif as video', () => {
    const result = makeTweetResult({ mediaEntities: [makeGifMedia()] });
    expect(extractTweet(result).mediaType).toBe('video');
  });

  it('detects multiple images as "images"', () => {
    const result = makeTweetResult({
      mediaEntities: [makeImageMedia('https://pbs.twimg.com/media/a.jpg'), makeImageMedia('https://pbs.twimg.com/media/b.jpg')],
    });
    expect(extractTweet(result).mediaType).toBe('images');
  });

  it('returns mediaType "text" for a tweet with no media', () => {
    const result = makeTweetResult({ mediaEntities: [] });
    expect(extractTweet(result).mediaType).toBe('text');
  });

  it('handles a wrapped tweet (result.tweet shape)', () => {
    const inner = makeTweetResult({ restId: '999', screenName: 'bob', name: 'Bob B', fullText: 'Wrapped' });
    const wrapped = { tweet: inner };
    const item = extractTweet(wrapped);
    expect(item.id).toBe('999');
    expect(item.authorUsername).toBe('bob');
    expect(item.text).toBe('Wrapped');
  });

  it('returns null when tweetId is missing', () => {
    const result = {
      rest_id: '',
      legacy: {},
      core: { user_results: { result: { legacy: {} } } },
    };
    expect(extractTweet(result)).toBeNull();
  });

  it('returns null for null input', () => {
    expect(extractTweet(null)).toBeNull();
  });

  it('leaves authorUsername and authorName empty when user info is absent', () => {
    const result = {
      rest_id: '777',
      legacy: { id_str: '777', full_text: 'No user' },
    };
    const item = extractTweet(result);
    expect(item.authorUsername).toBe('');
    expect(item.authorName).toBe('');
    expect(item.profileUrl).toBe('');
    // yt-dlp rejects `x.com//status/<id>`; the `i` placeholder is a valid URL.
    expect(item.postUrl).toBe('https://x.com/i/status/777');
  });

  it('parses the timestamp into an ISO string', () => {
    const createdAt = 'Wed Apr 10 12:00:00 +0000 2024';
    const result = makeTweetResult({ createdAt });
    const item = extractTweet(result);
    expect(item.timestamp).toBe(new Date(createdAt).toISOString());
  });

  it('leaves timestamp empty when created_at is absent', () => {
    const result = makeTweetResult({ createdAt: '' });
    const item = extractTweet(result);
    expect(item.timestamp).toBe('');
  });

  it('uses profile_image_url_https as thumbnailUrl fallback when no media', () => {
    const result = {
      rest_id: '888',
      legacy: { id_str: '888', full_text: 'Profile pic fallback', created_at: '' },
      core: {
        user_results: {
          result: {
            legacy: {
              screen_name: 'charlie',
              name: 'Charlie C',
              profile_image_url_https: 'https://pbs.twimg.com/profile_images/pic.jpg',
            },
          },
        },
      },
    };
    const item = extractTweet(result);
    expect(item.thumbnailUrl).toBe('https://pbs.twimg.com/profile_images/pic.jpg');
  });
});

// ---------------------------------------------------------------------------
// parseBookmarkResponse
// ---------------------------------------------------------------------------

function makeBookmarkEntry(tweetResult, entryId = 'tweet-1') {
  return {
    entryId,
    content: {
      itemContent: {
        tweet_results: { result: tweetResult },
      },
    },
  };
}

function makeCursorEntry(cursorValue, entryId = 'cursor-bottom-1') {
  return {
    entryId,
    content: {
      cursorType: 'Bottom',
      value: cursorValue,
    },
  };
}

describe('parseBookmarkResponse', () => {
  it('parses tweets from bookmark_timeline_v2 format', () => {
    const tweetResult = makeTweetResult({ restId: 'bm1', screenName: 'diane', fullText: 'Bookmark v2' });
    const data = {
      data: {
        bookmark_timeline_v2: {
          timeline: {
            instructions: [
              {
                type: 'TimelineAddEntries',
                entries: [makeBookmarkEntry(tweetResult, 'tweet-bm1')],
              },
            ],
          },
        },
      },
    };

    const { items, hasNextPage } = parseBookmarkResponse(data);

    expect(items).toHaveLength(1);
    expect(items[0].id).toBe('bm1');
    expect(items[0].authorUsername).toBe('diane');
    expect(hasNextPage).toBeNull();
  });

  it('falls back to bookmarks_timeline format', () => {
    const tweetResult = makeTweetResult({ restId: 'bm2', screenName: 'eli', fullText: 'Fallback format' });
    const data = {
      data: {
        bookmarks_timeline: {
          timeline: {
            instructions: [
              {
                type: 'TimelineAddEntries',
                entries: [makeBookmarkEntry(tweetResult, 'tweet-bm2')],
              },
            ],
          },
        },
      },
    };

    const { items } = parseBookmarkResponse(data);

    expect(items).toHaveLength(1);
    expect(items[0].id).toBe('bm2');
  });

  it('detects cursor-bottom entry and sets hasNextPage true when cursor has a value', () => {
    const tweetResult = makeTweetResult({ restId: 'bm3', screenName: 'faye' });
    const data = {
      data: {
        bookmark_timeline_v2: {
          timeline: {
            instructions: [
              {
                type: 'TimelineAddEntries',
                entries: [
                  makeBookmarkEntry(tweetResult, 'tweet-bm3'),
                  makeCursorEntry('abc_cursor', 'cursor-bottom-0'),
                ],
              },
            ],
          },
        },
      },
    };

    const { items, hasNextPage } = parseBookmarkResponse(data);

    expect(items).toHaveLength(1);
    expect(hasNextPage).toBe(true);
  });

  it('sets hasNextPage false when cursor entry has no value', () => {
    const tweetResult = makeTweetResult({ restId: 'bm4', screenName: 'glen' });
    const data = {
      data: {
        bookmark_timeline_v2: {
          timeline: {
            instructions: [
              {
                type: 'TimelineAddEntries',
                entries: [
                  makeBookmarkEntry(tweetResult, 'tweet-bm4'),
                  makeCursorEntry('', 'cursor-bottom-0'),
                ],
              },
            ],
          },
        },
      },
    };

    const { hasNextPage } = parseBookmarkResponse(data);

    expect(hasNextPage).toBe(false);
  });

  it('sets hasNextPage false when instructions exist but contain no tweets', () => {
    const data = {
      data: {
        bookmark_timeline_v2: {
          timeline: {
            instructions: [
              {
                type: 'TimelineAddEntries',
                entries: [],
              },
            ],
          },
        },
      },
    };

    const { items, hasNextPage } = parseBookmarkResponse(data);

    expect(items).toEqual([]);
    expect(hasNextPage).toBe(false);
  });

  it('returns empty items and null hasNextPage for empty data', () => {
    const { items, hasNextPage } = parseBookmarkResponse({});
    expect(items).toEqual([]);
    expect(hasNextPage).toBeNull();
  });

  it('skips entries whose type is not TimelineAddEntries', () => {
    const data = {
      data: {
        bookmark_timeline_v2: {
          timeline: {
            instructions: [
              { type: 'TimelineClearCache' },
            ],
          },
        },
      },
    };

    const { items } = parseBookmarkResponse(data);

    expect(items).toEqual([]);
  });
});

// ---------------------------------------------------------------------------
// normalizeExportedPost
// ---------------------------------------------------------------------------

describe('normalizeExportedPost', () => {
  it('preserves all explicit fields', () => {
    const post = {
      id: 'n1',
      authorUsername: 'hana',
      authorName: 'Hana H',
      postUrl: 'https://x.com/hana/status/n1',
      profileUrl: 'https://x.com/hana',
      text: 'Normalized text',
      thumbnailUrl: 'https://pbs.twimg.com/img.jpg',
      mediaType: 'image',
      timestamp: '2024-01-01T00:00:00.000Z',
    };
    const result = normalizeExportedPost(post);
    expect(result).toMatchObject(post);
  });

  it('infers postUrl from authorUsername and id', () => {
    const post = { id: 'n2', authorUsername: 'ivan' };
    const result = normalizeExportedPost(post);
    expect(result.postUrl).toBe('https://x.com/ivan/status/n2');
  });

  it('infers profileUrl from authorUsername', () => {
    const post = { id: 'n3', authorUsername: 'julia' };
    const result = normalizeExportedPost(post);
    expect(result.profileUrl).toBe('https://x.com/julia');
  });

  it('defaults mediaType to "text"', () => {
    const post = { id: 'n4', authorUsername: 'karl' };
    const result = normalizeExportedPost(post);
    expect(result.mediaType).toBe('text');
  });

  it('sets platform to twitter', () => {
    const post = { id: 'n5', authorUsername: 'lena' };
    const result = normalizeExportedPost(post);
    expect(result.platform).toBe('twitter');
  });

  it('sets shortcode to empty string', () => {
    const post = { id: 'n6', authorUsername: 'mike' };
    const result = normalizeExportedPost(post);
    expect(result.shortcode).toBe('');
  });

  it('preserves text field', () => {
    const post = { id: 'n7', authorUsername: 'nina', text: 'Tweet text here' };
    const result = normalizeExportedPost(post);
    expect(result.text).toBe('Tweet text here');
  });
});

// ---------------------------------------------------------------------------
// extractTweet — media array (multi-image tweets)
// ---------------------------------------------------------------------------

describe('extractTweet – media array', () => {
  it('returns one media entry per image in a multi-image tweet', () => {
    const result = extractTweet(
      makeTweetResult({
        mediaEntities: [
          makeImageMedia('https://pbs.twimg.com/media/1.jpg'),
          makeImageMedia('https://pbs.twimg.com/media/2.jpg'),
          makeImageMedia('https://pbs.twimg.com/media/3.jpg'),
        ],
      })
    );
    expect(result.mediaType).toBe('images');
    expect(result.media).toEqual([
      { type: 'image', url: 'https://pbs.twimg.com/media/1.jpg' },
      { type: 'image', url: 'https://pbs.twimg.com/media/2.jpg' },
      { type: 'image', url: 'https://pbs.twimg.com/media/3.jpg' },
    ]);
  });

  it('uses the first media url as the cover thumbnail', () => {
    const result = extractTweet(
      makeTweetResult({
        mediaEntities: [
          makeImageMedia('https://pbs.twimg.com/media/first.jpg'),
          makeImageMedia('https://pbs.twimg.com/media/second.jpg'),
        ],
      })
    );
    expect(result.thumbnailUrl).toBe('https://pbs.twimg.com/media/first.jpg');
  });

  it('classifies video/gif entries as video in the media array', () => {
    const result = extractTweet(
      makeTweetResult({ mediaEntities: [makeVideoMedia(), makeGifMedia()] })
    );
    expect(result.media.map((m) => m.type)).toEqual(['video', 'video']);
  });

  it('produces a single-entry media array for a one-image tweet', () => {
    const result = extractTweet(makeTweetResult({ mediaEntities: [makeImageMedia()] }));
    expect(result.mediaType).toBe('image');
    expect(result.media).toHaveLength(1);
  });

  it('produces an empty media array for a text-only tweet', () => {
    const result = extractTweet(makeTweetResult({ mediaEntities: [] }));
    expect(result.media).toEqual([]);
  });
});
