import{existsSync as X,mkdirSync as RI,writeFileSync as TI,readFileSync as x}from"fs";import{join as m,resolve as Z}from"path";import{spawn as SI}from"child_process";import{createHash as NI}from"crypto";import{generate as AI}from"orval";/*!
 * Copyright (c) Squirrel Chat et al., All rights reserved.
 * SPDX-License-Identifier: BSD-3-Clause
 *
 * Redistribution and use in source and binary forms, with or without
 * modification, are permitted provided that the following conditions are met:
 *
 * 1. Redistributions of source code must retain the above copyright notice, this
 *    list of conditions and the following disclaimer.
 * 2. Redistributions in binary form must reproduce the above copyright notice,
 *    this list of conditions and the following disclaimer in the
 *    documentation and/or other materials provided with the distribution.
 * 3. Neither the name of the copyright holder nor the names of its contributors
 *    may be used to endorse or promote products derived from this software without
 *    specific prior written permission.
 *
 * THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND
 * ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED
 * WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
 * DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE
 * FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
 * DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
 * SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
 * CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
 * OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
 * OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
 */function v(I,i){let T=I.slice(0,i).split(/\r\n|\n|\r/g);return[T.length,T.pop().length+1]}function a(I,i,T){let R=I.split(/\r\n|\n|\r/g),E="",S=(Math.log10(i+1)|0)+1;for(let O=i-1;O<=i+1;O++){let N=R[O-1];if(!N)continue;if(E+=O.toString().padEnd(S," "),E+=":  ",E+=N,E+=`
`,O===i)E+=" ".repeat(S+T+2),E+=`^
`}return E}class n extends Error{line;column;codeblock;constructor(I,i){let[T,R]=v(i.toml,i.ptr),E=a(i.toml,T,R);super(`Invalid TOML document: ${I}

${E}`,i);this.line=T,this.column=R,this.codeblock=E}}/*!
 * Copyright (c) Squirrel Chat et al., All rights reserved.
 * SPDX-License-Identifier: BSD-3-Clause
 *
 * Redistribution and use in source and binary forms, with or without
 * modification, are permitted provided that the following conditions are met:
 *
 * 1. Redistributions of source code must retain the above copyright notice, this
 *    list of conditions and the following disclaimer.
 * 2. Redistributions in binary form must reproduce the above copyright notice,
 *    this list of conditions and the following disclaimer in the
 *    documentation and/or other materials provided with the distribution.
 * 3. Neither the name of the copyright holder nor the names of its contributors
 *    may be used to endorse or promote products derived from this software without
 *    specific prior written permission.
 *
 * THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND
 * ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED
 * WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
 * DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE
 * FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
 * DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
 * SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
 * CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
 * OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
 * OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
 */function t(I,i){let T=0;while(I[i-++T]==="\\");return--T&&T%2}function F(I,i=0,T=I.length){let R=I.indexOf(`
`,i);if(I[R-1]==="\r")R--;return R<=T?R:-1}function l(I,i){for(let T=i;T<I.length;T++){let R=I[T];if(R===`
`)return T;if(R==="\r"&&I[T+1]===`
`)return T+1;if(R<" "&&R!=="\t"||R==="")throw new n("control characters are not allowed in comments",{toml:I,ptr:i})}return I.length}function D(I,i,T,R){let E;while((E=I[i])===" "||E==="\t"||!T&&(E===`
`||E==="\r"&&I[i+1]===`
`))i++;return R||E!=="#"?i:D(I,l(I,i),T)}function V(I,i,T,R,E=!1){if(!R)return i=F(I,i),i<0?I.length:i;for(let S=i;S<I.length;S++){let O=I[S];if(O==="#")S=F(I,S);else if(O===T)return S+1;else if(O===R||E&&(O===`
`||O==="\r"&&I[S+1]===`
`))return S}throw new n("cannot find end of structure",{toml:I,ptr:i})}function y(I,i){let T=I[i],R=T===I[i+1]&&I[i+1]===I[i+2]?I.slice(i,i+3):T;i+=R.length-1;do i=I.indexOf(R,++i);while(i>-1&&T!=="'"&&t(I,i));if(i>-1){if(i+=R.length,R.length>1){if(I[i]===T)i++;if(I[i]===T)i++}}return i}/*!
 * Copyright (c) Squirrel Chat et al., All rights reserved.
 * SPDX-License-Identifier: BSD-3-Clause
 *
 * Redistribution and use in source and binary forms, with or without
 * modification, are permitted provided that the following conditions are met:
 *
 * 1. Redistributions of source code must retain the above copyright notice, this
 *    list of conditions and the following disclaimer.
 * 2. Redistributions in binary form must reproduce the above copyright notice,
 *    this list of conditions and the following disclaimer in the
 *    documentation and/or other materials provided with the distribution.
 * 3. Neither the name of the copyright holder nor the names of its contributors
 *    may be used to endorse or promote products derived from this software without
 *    specific prior written permission.
 *
 * THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND
 * ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED
 * WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
 * DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE
 * FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
 * DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
 * SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
 * CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
 * OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
 * OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
 */var r=/^(\d{4}-\d{2}-\d{2})?[T ]?(?:(\d{2}):\d{2}:\d{2}(?:\.\d+)?)?(Z|[-+]\d{2}:\d{2})?$/i;class b extends Date{#i=!1;#E=!1;#I=null;constructor(I){let i=!0,T=!0,R="Z";if(typeof I==="string"){let E=I.match(r);if(E){if(!E[1])i=!1,I=`0000-01-01T${I}`;if(T=!!E[2],T&&I[10]===" "&&(I=I.replace(" ","T")),E[2]&&+E[2]>23)I="";else if(R=E[3]||null,I=I.toUpperCase(),!R&&T)I+="Z"}else I=""}super(I);if(!isNaN(this.getTime()))this.#i=i,this.#E=T,this.#I=R}isDateTime(){return this.#i&&this.#E}isLocal(){return!this.#i||!this.#E||!this.#I}isDate(){return this.#i&&!this.#E}isTime(){return this.#E&&!this.#i}isValid(){return this.#i||this.#E}toISOString(){let I=super.toISOString();if(this.isDate())return I.slice(0,10);if(this.isTime())return I.slice(11,23);if(this.#I===null)return I.slice(0,-1);if(this.#I==="Z")return I;let i=+this.#I.slice(1,3)*60+ +this.#I.slice(4,6);return i=this.#I[0]==="-"?i:-i,new Date(this.getTime()-i*60000).toISOString().slice(0,-1)+this.#I}static wrapAsOffsetDateTime(I,i="Z"){let T=new b(I);return T.#I=i,T}static wrapAsLocalDateTime(I){let i=new b(I);return i.#I=null,i}static wrapAsLocalDate(I){let i=new b(I);return i.#E=!1,i.#I=null,i}static wrapAsLocalTime(I){let i=new b(I);return i.#i=!1,i.#I=null,i}}/*!
 * Copyright (c) Squirrel Chat et al., All rights reserved.
 * SPDX-License-Identifier: BSD-3-Clause
 *
 * Redistribution and use in source and binary forms, with or without
 * modification, are permitted provided that the following conditions are met:
 *
 * 1. Redistributions of source code must retain the above copyright notice, this
 *    list of conditions and the following disclaimer.
 * 2. Redistributions in binary form must reproduce the above copyright notice,
 *    this list of conditions and the following disclaimer in the
 *    documentation and/or other materials provided with the distribution.
 * 3. Neither the name of the copyright holder nor the names of its contributors
 *    may be used to endorse or promote products derived from this software without
 *    specific prior written permission.
 *
 * THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND
 * ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED
 * WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
 * DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE
 * FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
 * DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
 * SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
 * CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
 * OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
 * OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
 */var p=/^((0x[0-9a-fA-F](_?[0-9a-fA-F])*)|(([+-]|0[ob])?\d(_?\d)*))$/,s=/^[+-]?\d(_?\d)*(\.\d(_?\d)*)?([eE][+-]?\d(_?\d)*)?$/,II=/^[+-]?0[0-9_]/,iI=/^[0-9a-f]{4,8}$/i,q={b:"\b",t:"\t",n:`
`,f:"\f",r:"\r",'"':'"',"\\":"\\"};function M(I,i=0,T=I.length){let R=I[i]==="'",E=I[i++]===I[i]&&I[i]===I[i+1];if(E){if(T-=2,I[i+=2]==="\r")i++;if(I[i]===`
`)i++}let S=0,O,N="",f=i;while(i<T-1){let A=I[i++];if(A===`
`||A==="\r"&&I[i]===`
`){if(!E)throw new n("newlines are not allowed in strings",{toml:I,ptr:i-1})}else if(A<" "&&A!=="\t"||A==="")throw new n("control characters are not allowed in strings",{toml:I,ptr:i-1});if(O){if(O=!1,A==="u"||A==="U"){let L=I.slice(i,i+=A==="u"?4:8);if(!iI.test(L))throw new n("invalid unicode escape",{toml:I,ptr:S});try{N+=String.fromCodePoint(parseInt(L,16))}catch{throw new n("invalid unicode escape",{toml:I,ptr:S})}}else if(E&&(A===`
`||A===" "||A==="\t"||A==="\r")){if(i=D(I,i-1,!0),I[i]!==`
`&&I[i]!=="\r")throw new n("invalid escape: only line-ending whitespace may be escaped",{toml:I,ptr:S});i=D(I,i)}else if(A in q)N+=q[A];else throw new n("unrecognized escape sequence",{toml:I,ptr:S});f=i}else if(!R&&A==="\\")S=i-1,O=!0,N+=I.slice(f,S)}return N+I.slice(f,T-1)}function Q(I,i,T,R){if(I==="true")return!0;if(I==="false")return!1;if(I==="-inf")return-1/0;if(I==="inf"||I==="+inf")return 1/0;if(I==="nan"||I==="+nan"||I==="-nan")return NaN;if(I==="-0")return R?0n:0;let E=p.test(I);if(E||s.test(I)){if(II.test(I))throw new n("leading zeroes are not allowed",{toml:i,ptr:T});I=I.replace(/_/g,"");let O=+I;if(isNaN(O))throw new n("invalid number",{toml:i,ptr:T});if(E){if((E=!Number.isSafeInteger(O))&&!R)throw new n("integer value cannot be represented losslessly",{toml:i,ptr:T});if(E||R===!0)O=BigInt(I)}return O}let S=new b(I);if(!S.isValid())throw new n("invalid value",{toml:i,ptr:T});return S}/*!
 * Copyright (c) Squirrel Chat et al., All rights reserved.
 * SPDX-License-Identifier: BSD-3-Clause
 *
 * Redistribution and use in source and binary forms, with or without
 * modification, are permitted provided that the following conditions are met:
 *
 * 1. Redistributions of source code must retain the above copyright notice, this
 *    list of conditions and the following disclaimer.
 * 2. Redistributions in binary form must reproduce the above copyright notice,
 *    this list of conditions and the following disclaimer in the
 *    documentation and/or other materials provided with the distribution.
 * 3. Neither the name of the copyright holder nor the names of its contributors
 *    may be used to endorse or promote products derived from this software without
 *    specific prior written permission.
 *
 * THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND
 * ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED
 * WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
 * DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE
 * FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
 * DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
 * SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
 * CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
 * OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
 * OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
 */function EI(I,i,T,R){let E=I.slice(i,T),S=E.indexOf("#");if(S>-1)l(I,S),E=E.slice(0,S);let O=E.trimEnd();if(!R){let N=E.indexOf(`
`,O.length);if(N>-1)throw new n("newlines are not allowed in inline tables",{toml:I,ptr:i+N})}return[O,S]}function e(I,i,T,R,E){if(R===0)throw new n("document contains excessively nested structures. aborting.",{toml:I,ptr:i});let S=I[i];if(S==="["||S==="{"){let[f,A]=S==="["?J(I,i,R,E):z(I,i,R,E),L=T?V(I,A,",",T):A;if(A-L&&T==="}"){let u=F(I,A,L);if(u>-1)throw new n("newlines are not allowed in inline tables",{toml:I,ptr:u})}return[f,L]}let O;if(S==='"'||S==="'"){O=y(I,i);let f=M(I,i,O);if(T){if(O=D(I,O,T!=="]"),I[O]&&I[O]!==","&&I[O]!==T&&I[O]!==`
`&&I[O]!=="\r")throw new n("unexpected character encountered",{toml:I,ptr:O});O+=+(I[O]===",")}return[f,O]}O=V(I,i,",",T);let N=EI(I,i,O-+(I[O-1]===","),T==="]");if(!N[0])throw new n("incomplete key-value declaration: no value specified",{toml:I,ptr:i});if(T&&N[1]>-1)O=D(I,i+N[1]),O+=+(I[O]===",");return[Q(N[0],I,i,E),O]}/*!
 * Copyright (c) Squirrel Chat et al., All rights reserved.
 * SPDX-License-Identifier: BSD-3-Clause
 *
 * Redistribution and use in source and binary forms, with or without
 * modification, are permitted provided that the following conditions are met:
 *
 * 1. Redistributions of source code must retain the above copyright notice, this
 *    list of conditions and the following disclaimer.
 * 2. Redistributions in binary form must reproduce the above copyright notice,
 *    this list of conditions and the following disclaimer in the
 *    documentation and/or other materials provided with the distribution.
 * 3. Neither the name of the copyright holder nor the names of its contributors
 *    may be used to endorse or promote products derived from this software without
 *    specific prior written permission.
 *
 * THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND
 * ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED
 * WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
 * DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE
 * FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
 * DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
 * SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
 * CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
 * OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
 * OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
 */var OI=/^[a-zA-Z0-9-_]+[ \t]*$/;function G(I,i,T="="){let R=i-1,E=[],S=I.indexOf(T,i);if(S<0)throw new n("incomplete key-value: cannot find end of key",{toml:I,ptr:i});do{let O=I[i=++R];if(O!==" "&&O!=="\t")if(O==='"'||O==="'"){if(O===I[i+1]&&O===I[i+2])throw new n("multiline strings are not allowed in keys",{toml:I,ptr:i});let N=y(I,i);if(N<0)throw new n("unfinished string encountered",{toml:I,ptr:i});R=I.indexOf(".",N);let f=I.slice(N,R<0||R>S?S:R),A=F(f);if(A>-1)throw new n("newlines are not allowed in keys",{toml:I,ptr:i+R+A});if(f.trimStart())throw new n("found extra tokens after the string part",{toml:I,ptr:N});if(S<N){if(S=I.indexOf(T,N),S<0)throw new n("incomplete key-value: cannot find end of key",{toml:I,ptr:i})}E.push(M(I,i,N))}else{R=I.indexOf(".",i);let N=I.slice(i,R<0||R>S?S:R);if(!OI.test(N))throw new n("only letter, numbers, dashes and underscores are allowed in keys",{toml:I,ptr:i});E.push(N.trimEnd())}}while(R+1&&R<S);return[E,D(I,S+1,!0,!0)]}function z(I,i,T,R){let E={},S=new Set,O,N=0;i++;while((O=I[i++])!=="}"&&O){let f={toml:I,ptr:i-1};if(O===`
`)throw new n("newlines are not allowed in inline tables",f);else if(O==="#")throw new n("inline tables cannot contain comments",f);else if(O===",")throw new n("expected key-value, found comma",f);else if(O!==" "&&O!=="\t"){let A,L=E,u=!1,[w,h]=G(I,i-1);for(let o=0;o<w.length;o++){if(o)L=u?L[A]:L[A]={};if(A=w[o],(u=Object.hasOwn(L,A))&&(typeof L[A]!=="object"||S.has(L[A])))throw new n("trying to redefine an already defined value",{toml:I,ptr:i});if(!u&&A==="__proto__")Object.defineProperty(L,A,{enumerable:!0,configurable:!0,writable:!0})}if(u)throw new n("trying to redefine an already defined value",{toml:I,ptr:i});let[B,W]=e(I,h,"}",T-1,R);S.add(B),L[A]=B,i=W,N=I[i-1]===","?i-1:0}}if(N)throw new n("trailing commas are not allowed in inline tables",{toml:I,ptr:N});if(!O)throw new n("unfinished table encountered",{toml:I,ptr:i});return[E,i]}function J(I,i,T,R){let E=[],S;i++;while((S=I[i++])!=="]"&&S)if(S===",")throw new n("expected value, found comma",{toml:I,ptr:i-1});else if(S==="#")i=l(I,i);else if(S!==" "&&S!=="\t"&&S!==`
`&&S!=="\r"){let O=e(I,i-1,"]",T-1,R);E.push(O[0]),i=O[1]}if(!S)throw new n("unfinished array encountered",{toml:I,ptr:i});return[E,i]}/*!
 * Copyright (c) Squirrel Chat et al., All rights reserved.
 * SPDX-License-Identifier: BSD-3-Clause
 *
 * Redistribution and use in source and binary forms, with or without
 * modification, are permitted provided that the following conditions are met:
 *
 * 1. Redistributions of source code must retain the above copyright notice, this
 *    list of conditions and the following disclaimer.
 * 2. Redistributions in binary form must reproduce the above copyright notice,
 *    this list of conditions and the following disclaimer in the
 *    documentation and/or other materials provided with the distribution.
 * 3. Neither the name of the copyright holder nor the names of its contributors
 *    may be used to endorse or promote products derived from this software without
 *    specific prior written permission.
 *
 * THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND
 * ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED
 * WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
 * DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE
 * FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
 * DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
 * SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
 * CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
 * OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
 * OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
 */function K(I,i,T,R){let E=i,S=T,O,N=!1,f;for(let A=0;A<I.length;A++){if(A){if(E=N?E[O]:E[O]={},S=(f=S[O]).c,R===0&&(f.t===1||f.t===2))return null;if(f.t===2){let L=E.length-1;E=E[L],S=S[L].c}}if(O=I[A],(N=Object.hasOwn(E,O))&&S[O]?.t===0&&S[O]?.d)return null;if(!N){if(O==="__proto__")Object.defineProperty(E,O,{enumerable:!0,configurable:!0,writable:!0}),Object.defineProperty(S,O,{enumerable:!0,configurable:!0,writable:!0});S[O]={t:A<I.length-1&&R===2?3:R,d:!1,i:0,c:{}}}}if(f=S[O],f.t!==R&&!(R===1&&f.t===3))return null;if(R===2){if(!f.d)f.d=!0,E[O]=[];E[O].push(E={}),f.c[f.i++]=f={t:1,d:!1,i:0,c:{}}}if(f.d)return null;if(f.d=!0,R===1)E=N?E[O]:E[O]={};else if(R===0&&N)return null;return[O,E,f.c]}function g(I,{maxDepth:i=1000,integersAsBigInt:T}={}){let R={},E={},S=R,O=E;for(let N=D(I,0);N<I.length;){if(I[N]==="["){let f=I[++N]==="[",A=G(I,N+=+f,"]");if(f){if(I[A[1]-1]!=="]")throw new n("expected end of table declaration",{toml:I,ptr:A[1]-1});A[1]++}let L=K(A[0],R,E,f?2:1);if(!L)throw new n("trying to redefine an already defined table or value",{toml:I,ptr:N});O=L[2],S=L[1],N=A[1]}else{let f=G(I,N),A=K(f[0],S,O,0);if(!A)throw new n("trying to redefine an already defined table or value",{toml:I,ptr:N});let L=e(I,f[1],void 0,i,T);A[1][A[0]]=L[0],N=L[1]}if(N=D(I,N,!0),I[N]&&I[N]!==`
`&&I[N]!=="\r")throw new n("each key-value declaration must be followed by an end-of-line",{toml:I,ptr:N});N=D(I,N)}return R}/*!
 * Copyright (c) Squirrel Chat et al., All rights reserved.
 * SPDX-License-Identifier: BSD-3-Clause
 *
 * Redistribution and use in source and binary forms, with or without
 * modification, are permitted provided that the following conditions are met:
 *
 * 1. Redistributions of source code must retain the above copyright notice, this
 *    list of conditions and the following disclaimer.
 * 2. Redistributions in binary form must reproduce the above copyright notice,
 *    this list of conditions and the following disclaimer in the
 *    documentation and/or other materials provided with the distribution.
 * 3. Neither the name of the copyright holder nor the names of its contributors
 *    may be used to endorse or promote products derived from this software without
 *    specific prior written permission.
 *
 * THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND
 * ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED
 * WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
 * DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE
 * FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
 * DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
 * SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
 * CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
 * OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
 * OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
 *//*!
 * Copyright (c) Squirrel Chat et al., All rights reserved.
 * SPDX-License-Identifier: BSD-3-Clause
 *
 * Redistribution and use in source and binary forms, with or without
 * modification, are permitted provided that the following conditions are met:
 *
 * 1. Redistributions of source code must retain the above copyright notice, this
 *    list of conditions and the following disclaimer.
 * 2. Redistributions in binary form must reproduce the above copyright notice,
 *    this list of conditions and the following disclaimer in the
 *    documentation and/or other materials provided with the distribution.
 * 3. Neither the name of the copyright holder nor the names of its contributors
 *    may be used to endorse or promote products derived from this software without
 *    specific prior written permission.
 *
 * THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND
 * ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED
 * WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
 * DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE
 * FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
 * DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
 * SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
 * CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
 * OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
 * OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
 */var _=new Map,kI=(I)=>I,jI=(I,i)=>({name:"openapi",action:`uv run --no-sync apx openapi ${I} ${i}`}),vI=({input:I,output:i})=>({name:"orval",action:async()=>{if(!X(I)){console.warn(`[apx] OpenAPI spec not found at ${I}, skipping Orval generation`);return}let T=x(I,"utf-8"),R=NI("sha256").update(T).digest("hex");if(_.get(I)===R){console.log("[apx] OpenAPI spec unchanged, skipping Orval generation");return}await AI({input:I,output:i}),_.set(I,R)}});function fI(I={}){let{steps:i=[],ignore:T=[]}=I,R,E=null,S=!1,O=[],N=!1,f=[];function A(o){return new Promise((U,Y)=>{if(S){console.log(`[apx] Skipping command (stopping): ${o}`),U();return}console.log(`[apx] Executing: ${o}`);let $=o.split(/\s+/),d=$[0],k=$.slice(1),P=process.env.APX_PIPE_OUTPUT==="1",C=SI(d,k,{stdio:P?"pipe":"inherit",shell:!0,detached:!1});if(P&&C.stdout&&C.stderr)C.stdout.on("data",(H)=>{process.stdout.write(H)}),C.stderr.on("data",(H)=>{process.stderr.write(H)});if(f.push(C),C.on("error",(H)=>{console.error("[apx] Process error:",H),Y(H)}),C.on("exit",(H,c)=>{if(f=f.filter((j)=>j.pid!==C.pid),c)console.log(`[apx] Process ${C.pid} exited with signal: ${c}`),U();else if(H!==0)console.error(`[apx] Process ${C.pid} exited with code: ${H}`),Y(Error(`Command failed with exit code ${H}`));else U()}),S&&C.pid)console.log(`[apx] Killing process ${C.pid} (stopping)`),L(C)})}function L(o){if(!o.pid)return;try{if(process.platform!=="win32")process.kill(-o.pid,"SIGTERM"),console.log(`[apx] Sent SIGTERM to process group -${o.pid}`);else o.kill("SIGTERM"),console.log(`[apx] Sent SIGTERM to process ${o.pid}`)}catch(U){console.error(`[apx] Error killing process ${o.pid}:`,U);try{o.kill("SIGKILL")}catch(Y){}}}async function u(o){if(S){console.log("[apx] Skipping action (stopping)");return}if(h(),typeof o==="string")await A(o);else{if(S)return;await o()}h()}async function w(){if(S){console.log("[apx] Skipping steps (stopping)");return}if(N){console.log("[apx] Steps already running, skipping...");return}console.log(`[apx] Running ${i.length} step(s)...`),N=!0;try{for(let o of i){if(S){console.log("[apx] Stopping during step execution");break}let U=Date.now();try{console.log(`[apx] ${o.name} ⏳`),await u(o.action),console.log(`[apx] ${o.name} ✓ (${Date.now()-U} ms)`)}catch(Y){throw console.error(`[apx] ${o.name} ✗`,Y),Y}}console.log("[apx] All steps completed")}finally{N=!1}}function h(){if(!R){console.error("[apx] outDir is not set");return}try{if(!X(R))RI(R,{recursive:!0});let o=m(R,".gitignore");if(!X(o))TI(o,`*
`)}catch(o){console.error("[apx] failed to ensure output directory:",o)}}function B(){if(S)return;if(console.log(`[apx] Stopping... (${f.length} child processes)`),S=!0,E)clearTimeout(E),E=null;if(f.length>0)console.log(`[apx] Killing ${f.length} child process(es)...`),f.forEach((o)=>{if(o.pid)L(o)}),f=[];console.log("[apx] Stopped")}function W(){S=!1,E=null,N=!1,f=[]}return{name:"apx",apply:()=>!0,configResolved(o){R=Z(o.root,o.build.outDir),O=T.map((U)=>Z(process.cwd(),U)),W(),h()},configureServer(o){o.httpServer?.once("close",()=>{console.log("[apx] Server closing, stopping plugin..."),B()}),h()},async buildStart(){if(h(),i.length>0)await w()},handleHotUpdate(o){if(h(),S){console.log("[apx] HMR update ignored (stopping)");return}if(O.some((U)=>o.file.includes(U)))return;if(console.log(`[apx] HMR update detected: ${o.file}`),E)clearTimeout(E);E=setTimeout(async()=>{if(E=null,S)return;h(),await w(),h()},100),E.unref()},writeBundle(){h()},closeBundle(){h(),B()}}}function aI(){let I=m(process.cwd(),"pyproject.toml"),i=g(x(I,"utf-8")),T=typeof i==="object"&&i!==null&&"tool"in i?i.tool:void 0,R=T&&typeof T==="object"&&T!==null&&"apx"in T?T.apx:void 0,E=R&&typeof R==="object"&&R!==null&&"metadata"in R?R.metadata:void 0;if(!E||typeof E!=="object")throw Error("Could not find [tool.apx.metadata] in pyproject.toml");return{appName:E["app-name"],appModule:E["app-module"]}}var rI=fI;export{aI as readMetadata,rI as default,fI as apx,kI as Step,vI as Orval,jI as OpenAPI};

//# debugId=83F10B87CBAB342564756E2164756E21
//# sourceMappingURL=index.js.map
