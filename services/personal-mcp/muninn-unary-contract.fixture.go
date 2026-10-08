package mcp

import (
 "bytes"
 "encoding/json"
 "net/http"
 "net/http/httptest"
 "testing"
 "github.com/scrypster/muninndb/internal/auth"
)

// Isolated overlay fixture: actual /mcp handler, synthetic observe key/engine.
func TestPercivalUnaryContractWithoutSession(t *testing.T) {
 store := newMockKeyStore(auth.APIKey{ID:"percival_synthetic",Vault:"percival_connection_test",Mode:auth.ModeObserve})
 eng := &captureReadOnlyEngine{}
 srv := New(":0",eng,"",store,nil,nil)
 post := func(body []byte) *httptest.ResponseRecorder {
  r := httptest.NewRequest(http.MethodPost,"/mcp",bytes.NewReader(body))
  r.Header.Set("Authorization","Bearer mk_percival_synthetic")
  r.Header.Set("Content-Type","application/json")
  r.Header.Set("Accept","application/json")
  // Deliberately no initialize/session header and no SSE stream.
  w := httptest.NewRecorder(); srv.handleStreamablePost(w,r); return w
 }
 list := post([]byte(`{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}`))
 if list.Code != 200 || list.Header().Get("Content-Type") != "application/json" { t.Fatalf("unary list status/type: %d %s",list.Code,list.Header().Get("Content-Type")) }
 var response JSONRPCResponse
 if err := json.Unmarshal(list.Body.Bytes(),&response); err != nil || response.Error != nil {t.Fatal("unary list failed")}
 recall := post(mkToolCallBody("muninn_recall",map[string]any{"vault":"percival_connection_test","context":[]string{"approved synthetic marker"},"read_only":true,"limit":1}))
 if recall.Code != 200 {t.Fatal("unary recall status",recall.Code)}
 if err := json.Unmarshal(recall.Body.Bytes(),&response); err != nil || response.Error != nil {t.Fatal("unary recall failed")}
 if !eng.gotActivateReadOnly {t.Fatal("observe recall did not reach engine as read-only")}
 for _,body := range [][]byte{
  mkToolCallBody("muninn_recall",map[string]any{"vault":"user_likesjx","context":[]string{"synthetic"}}),
  mkToolCallBody("muninn_recall",map[string]any{"vault":"percival_connection_test","context":[]string{"synthetic"},"read_only":false}),
  mkToolCallBody("muninn_remember",map[string]any{"vault":"percival_connection_test","content":"synthetic"}),
 } {
  denied:=post(body); response=JSONRPCResponse{}
  if err:=json.Unmarshal(denied.Body.Bytes(),&response);err!=nil || response.Error==nil {t.Fatal("scope/mode denial absent")}
 }
}
