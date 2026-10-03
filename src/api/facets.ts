export interface LibraryFacet {
  value: string;
  count: number;
}
export interface LibraryFacets {
  category: LibraryFacet[];
  contentType: LibraryFacet[];
  status: LibraryFacet[];
  language: LibraryFacet[];
}
export interface LibraryFacetsApi {
  get(signal?: AbortSignal): Promise<LibraryFacets>;
  onChanged(listener: () => void): () => void;
}
