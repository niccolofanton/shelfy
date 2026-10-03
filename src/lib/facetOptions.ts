// Desktop facet defaults and the localized label vocabulary. The web drawer
// reads values/counts from GET /facets and uses these lists only for known labels.
// Unknown server values remain visible verbatim.

export interface FacetOption {
  value: string;
  // i18n key, resolved against the `filterDrawer` namespace at render
  // (`facetCategory.<value>` / `facetContentType.<value>`); AI status reuses
  // the existing `aiStatus*` scheme already in that namespace isn't needed,
  // its own `facetAiStatus.<value>` key is added instead.
  key: string;
}

export function categoryOptions(): FacetOption[] {
  return [
    { value: 'technology', key: 'technology' },
    { value: 'fintech', key: 'fintech' },
    { value: 'fashion', key: 'fashion' },
    { value: 'food-beverage', key: 'foodBeverage' },
    { value: 'real-estate', key: 'realEstate' },
    { value: 'health', key: 'health' },
    { value: 'education', key: 'education' },
    { value: 'gaming', key: 'gaming' },
    { value: 'travel', key: 'travel' },
    { value: 'b2b-software', key: 'b2bSoftware' },
    { value: 'nonprofit', key: 'nonprofit' },
    { value: 'crypto-web3', key: 'cryptoWeb3' },
    { value: 'architecture', key: 'architecture' },
    { value: 'automotive', key: 'automotive' },
    { value: 'media-entertainment', key: 'mediaEntertainment' },
    { value: 'ecommerce-retail', key: 'ecommerceRetail' },
    { value: 'marketing-agency', key: 'marketingAgency' },
    { value: 'sports', key: 'sports' },
    { value: 'beauty', key: 'beauty' },
    { value: 'other', key: 'other' },
  ];
}

export function contentTypeOptions(): FacetOption[] {
  return [
    { value: 'portfolio', key: 'portfolio' },
    { value: 'e-commerce', key: 'eCommerce' },
    { value: 'saas', key: 'saas' },
    { value: 'landing', key: 'landing' },
    { value: 'agency', key: 'agency' },
    { value: 'editorial', key: 'editorial' },
    { value: 'corporate', key: 'corporate' },
    { value: 'docs', key: 'docs' },
    { value: 'webapp', key: 'webapp' },
    { value: 'directory', key: 'directory' },
    { value: 'personal', key: 'personal' },
    { value: 'other', key: 'other' },
  ];
}

// The desktop's AI analysis lifecycle (types/domain.d.ts Shelfy.AiStatus) —
// a genuinely closed, stable enum, not a stand-in for a future endpoint.
export function aiStatusOptions(): FacetOption[] {
  return [
    { value: 'pending', key: 'pending' },
    { value: 'analyzing', key: 'analyzing' },
    { value: 'done', key: 'done' },
    { value: 'error', key: 'error' },
  ];
}
